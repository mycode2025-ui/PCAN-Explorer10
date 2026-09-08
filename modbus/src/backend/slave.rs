//! slave responsibilities extracted from modbus/src/backend.rs.
use super::*;

pub(super) enum SlaveEvent {
    Log { dir: &'static str, text: String },
    Changed,
}

pub(super) enum SlaveMsg {
    Edit {
        address: u16,
        text: String,
    },
    EditAt {
        area: Area,
        address: u16,
        text: String,
    },
    SetName {
        address: u16,
        name: String,
    },
    SetCellFormat {
        address: u16,
        format: Option<RegFormat>,
    },
    ExportCsv(String),
    SetView {
        area: Area,
        address: u16,
        quantity: u16,
        format: RegFormat,
    },
    SetScaling(Scaling),
    SetColors(ColorRules),
    SetValueNames(ValueNames),
    SimStart(SimCfg),
    SimStop,
    ToggleAutoInc {
        address: u16,
    },
}

pub(super) struct SlaveShared {
    pub(super) store: Mutex<DataStore>,
    pub(super) requests: AtomicU64,
    pub(super) events: mpsc::Sender<SlaveEvent>,
    pub(super) event_drops: AtomicU64,
    pub(super) event_high_watermark: AtomicUsize,
}

impl SlaveShared {
    pub(super) fn send_event(&self, event: SlaveEvent) {
        match self.events.try_send(event) {
            Ok(()) => {
                self.event_high_watermark.fetch_max(
                    self.events
                        .max_capacity()
                        .saturating_sub(self.events.capacity()),
                    Ordering::Relaxed,
                );
            }
            Err(_) => {
                self.event_drops.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

#[derive(Clone)]
pub(super) struct SlaveService {
    pub(super) shared: Arc<SlaveShared>,
    pub(super) unit_id: u8,
    pub(super) tcp: bool,
    pub(super) ignore_unit_id: bool,
}

impl Service for SlaveService {
    type Request = SlaveRequest<'static>;
    type Response = Option<Response>;
    type Exception = ExceptionCode;
    type Future = Ready<Result<Self::Response, Self::Exception>>;

    fn call(&self, req: Self::Request) -> Self::Future {
        let SlaveRequest { slave, request } = req;
        let for_us = if self.tcp {
            self.ignore_unit_id || slave == self.unit_id
        } else {
            slave == self.unit_id
        };
        let broadcast = !self.tcp && slave == 0;
        if !for_us && !broadcast {
            return ready(Ok(None));
        }

        self.shared.requests.fetch_add(1, Ordering::Relaxed);
        self.shared.send_event(SlaveEvent::Log {
            dir: "RX",
            text: describe_request(&request),
        });

        match handle_request(&self.shared, &request) {
            Ok(resp) => {
                if is_write(&request) {
                    self.shared.send_event(SlaveEvent::Changed);
                }
                ready(Ok(if broadcast { None } else { Some(resp) }))
            }
            Err(e) => {
                self.shared.send_event(SlaveEvent::Log {
                    dir: "ERR",
                    text: format!("Exception: {e:?}"),
                });
                ready(Err(e))
            }
        }
    }
}

pub(super) struct SlaveHandle {
    pub(super) ctrl: mpsc::Sender<SlaveMsg>,
    pub(super) server: JoinHandle<()>,
    pub(super) updater: JoinHandle<()>,
    pub(super) ui: UiSink,
}

impl SlaveHandle {
    pub(super) fn send(&self, m: SlaveMsg) {
        if self.ctrl.try_send(m).is_err() {
            self.ui
                .slave_message("Slave control queue full: command rejected");
        }
    }
    pub(super) fn stop(self) {
        self.server.abort();
        self.updater.abort();
    }
}

#[derive(Clone, Copy)]
pub(super) struct SlaveView {
    pub(super) area: Area,
    pub(super) address: u16,
    pub(super) quantity: u16,
    pub(super) format: RegFormat,
}

pub(super) fn bind_error_message(
    host: &str,
    port: u16,
    error: &std::io::Error,
    english: bool,
) -> String {
    let address_not_local =
        error.kind() == std::io::ErrorKind::AddrNotAvailable || error.raw_os_error() == Some(10049);

    if address_not_local {
        if english {
            return format!(
                "Start failed: {host} is not currently available for listening. Its network adapter may be disconnected, the IP address may not be active yet, or the address may not be assigned to this computer. Connect the adapter and wait for the IP address to become active, or use 0.0.0.0 to listen on all active interfaces."
            );
        }
        return format!(
            "启动失败：{host} 当前不能用于监听。对应网卡可能未连接、IP 地址尚未生效，或该地址未分配给本机。请连接网卡并等待 IP 生效，或使用 0.0.0.0 监听全部有效网卡。"
        );
    }

    if error.kind() == std::io::ErrorKind::AddrInUse {
        return if english {
            format!("Start failed: port {port} is already in use.")
        } else {
            format!("启动失败：端口 {port} 已被其他程序占用。")
        };
    }

    if error.kind() == std::io::ErrorKind::PermissionDenied {
        return if english {
            format!("Start failed: permission denied while listening on {host}:{port}.")
        } else {
            format!("启动失败：没有权限监听 {host}:{port}，请更换端口或以管理员身份运行。")
        };
    }

    if english {
        format!("Start failed: cannot listen on {host}:{port} ({error}).")
    } else {
        format!("启动失败：无法监听 {host}:{port}（{error}）。")
    }
}

pub(super) fn pdu_u16(data: &[u8], offset: usize) -> Option<u16> {
    let bytes: [u8; 2] = data.get(offset..offset + 2)?.try_into().ok()?;
    Some(u16::from_be_bytes(bytes))
}

pub(super) fn decode_server_request_pdu(pdu: &[u8]) -> Result<Request<'static>, ExceptionCode> {
    let malformed = || ExceptionCode::IllegalDataValue;
    if pdu.len() > 253 {
        return Err(malformed());
    }
    let function = *pdu.first().ok_or_else(malformed)?;
    let pair = |offset| Some((pdu_u16(pdu, offset)?, pdu_u16(pdu, offset + 2)?));
    match function {
        0x01 if pdu.len() == 5 => pair(1)
            .map(|(a, q)| Request::ReadCoils(a, q))
            .ok_or_else(malformed),
        0x02 if pdu.len() == 5 => pair(1)
            .map(|(a, q)| Request::ReadDiscreteInputs(a, q))
            .ok_or_else(malformed),
        0x03 if pdu.len() == 5 => pair(1)
            .map(|(a, q)| Request::ReadHoldingRegisters(a, q))
            .ok_or_else(malformed),
        0x04 if pdu.len() == 5 => pair(1)
            .map(|(a, q)| Request::ReadInputRegisters(a, q))
            .ok_or_else(malformed),
        0x05 if pdu.len() == 5 => {
            let (address, raw) = pair(1).ok_or_else(malformed)?;
            let value = match raw {
                0xFF00 => true,
                0x0000 => false,
                _ => return Err(malformed()),
            };
            Ok(Request::WriteSingleCoil(address, value))
        }
        0x06 if pdu.len() == 5 => pair(1)
            .map(|(a, v)| Request::WriteSingleRegister(a, v))
            .ok_or_else(malformed),
        0x0F => {
            let (address, quantity) = pair(1).ok_or_else(malformed)?;
            let byte_count = usize::from(*pdu.get(5).ok_or_else(malformed)?);
            let packed = pdu.get(6..6 + byte_count).ok_or_else(malformed)?;
            if pdu.len() != 6 + byte_count || byte_count != usize::from(quantity).div_ceil(8) {
                return Err(malformed());
            }
            let values = (0..usize::from(quantity))
                .map(|i| packed[i / 8] & (1 << (i % 8)) != 0)
                .collect::<Vec<_>>();
            Ok(Request::WriteMultipleCoils(address, Cow::Owned(values)))
        }
        0x10 => {
            let (address, quantity) = pair(1).ok_or_else(malformed)?;
            let byte_count = usize::from(*pdu.get(5).ok_or_else(malformed)?);
            let bytes = pdu.get(6..6 + byte_count).ok_or_else(malformed)?;
            if pdu.len() != 6 + byte_count || byte_count != usize::from(quantity) * 2 {
                return Err(malformed());
            }
            let values = bytes
                .chunks_exact(2)
                .map(|chunk| u16::from_be_bytes([chunk[0], chunk[1]]))
                .collect::<Vec<_>>();
            Ok(Request::WriteMultipleRegisters(address, Cow::Owned(values)))
        }
        0x16 if pdu.len() == 7 => {
            let address = pdu_u16(pdu, 1).ok_or_else(malformed)?;
            let and_mask = pdu_u16(pdu, 3).ok_or_else(malformed)?;
            let or_mask = pdu_u16(pdu, 5).ok_or_else(malformed)?;
            Ok(Request::MaskWriteRegister(address, and_mask, or_mask))
        }
        0x17 => {
            let read_address = pdu_u16(pdu, 1).ok_or_else(malformed)?;
            let read_quantity = pdu_u16(pdu, 3).ok_or_else(malformed)?;
            let write_address = pdu_u16(pdu, 5).ok_or_else(malformed)?;
            let write_quantity = pdu_u16(pdu, 7).ok_or_else(malformed)?;
            let byte_count = usize::from(*pdu.get(9).ok_or_else(malformed)?);
            let bytes = pdu.get(10..10 + byte_count).ok_or_else(malformed)?;
            if pdu.len() != 10 + byte_count || byte_count != usize::from(write_quantity) * 2 {
                return Err(malformed());
            }
            let values = bytes
                .chunks_exact(2)
                .map(|chunk| u16::from_be_bytes([chunk[0], chunk[1]]))
                .collect::<Vec<_>>();
            Ok(Request::ReadWriteMultipleRegisters(
                read_address,
                read_quantity,
                write_address,
                Cow::Owned(values),
            ))
        }
        0x01..=0x06 | 0x16 => Err(malformed()),
        _ => Err(ExceptionCode::IllegalFunction),
    }
}

pub(super) fn encode_server_response_pdu(
    function: u8,
    response: Result<Response, ExceptionCode>,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(253);
    let response = match response {
        Ok(response) => response,
        Err(exception) => {
            out.push(function | 0x80);
            out.push(exception.into());
            return out;
        }
    };
    out.push(response.function_code().value());
    match response {
        Response::ReadCoils(values) | Response::ReadDiscreteInputs(values) => {
            let byte_count = values.len().div_ceil(8);
            out.push(byte_count as u8);
            out.resize(2 + byte_count, 0);
            for (index, value) in values.into_iter().enumerate() {
                if value {
                    out[2 + index / 8] |= 1 << (index % 8);
                }
            }
        }
        Response::ReadInputRegisters(values)
        | Response::ReadHoldingRegisters(values)
        | Response::ReadWriteMultipleRegisters(values) => {
            out.push((values.len() * 2) as u8);
            for value in values {
                out.extend_from_slice(&value.to_be_bytes());
            }
        }
        Response::WriteSingleCoil(address, value) => {
            out.extend_from_slice(&address.to_be_bytes());
            out.extend_from_slice(&(if value { 0xFF00u16 } else { 0 }).to_be_bytes());
        }
        Response::WriteSingleRegister(address, value) => {
            out.extend_from_slice(&address.to_be_bytes());
            out.extend_from_slice(&value.to_be_bytes());
        }
        Response::WriteMultipleCoils(address, quantity)
        | Response::WriteMultipleRegisters(address, quantity) => {
            out.extend_from_slice(&address.to_be_bytes());
            out.extend_from_slice(&quantity.to_be_bytes());
        }
        Response::MaskWriteRegister(address, and_mask, or_mask) => {
            out.extend_from_slice(&address.to_be_bytes());
            out.extend_from_slice(&and_mask.to_be_bytes());
            out.extend_from_slice(&or_mask.to_be_bytes());
        }
        Response::ReportServerId(server_id, running, additional) => {
            out.push((additional.len() + 2) as u8);
            out.push(server_id);
            out.push(if running { 0xFF } else { 0 });
            out.extend_from_slice(&additional);
        }
        Response::Custom(_, bytes) => out.extend_from_slice(&bytes),
        Response::ReadDeviceIdentification(_) => {
            out.clear();
            out.push(function | 0x80);
            out.push(ExceptionCode::IllegalFunction.into());
        }
    }
    out
}

pub(super) fn modbus_crc16(data: &[u8]) -> u16 {
    let mut crc = 0xFFFFu16;
    for &byte in data {
        crc ^= u16::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xA001
            } else {
                crc >> 1
            };
        }
    }
    crc
}

pub(super) async fn serve_udp_slave(
    socket: tokio::net::UdpSocket,
    service: SlaveService,
    traffic: TrafficTx,
    rtu: bool,
) -> std::io::Result<()> {
    let mut buffer = [0u8; 260];
    loop {
        let (size, peer) = socket.recv_from(&mut buffer).await?;
        let frame = &buffer[..size];
        let _ = traffic.send((false, frame.to_vec()));

        let parsed = if rtu {
            if frame.len() < 4 {
                None
            } else {
                let body_len = frame.len() - 2;
                let expected = u16::from_le_bytes([frame[body_len], frame[body_len + 1]]);
                (modbus_crc16(&frame[..body_len]) == expected)
                    .then(|| (None, frame[0], &frame[1..body_len]))
            }
        } else if frame.len() >= 8 && frame[2..4] == [0, 0] {
            let length = usize::from(u16::from_be_bytes([frame[4], frame[5]]));
            let end = 6usize.saturating_add(length);
            (length >= 2 && end == frame.len())
                .then(|| (Some([frame[0], frame[1]]), frame[6], &frame[7..end]))
        } else {
            None
        };

        let Some((transaction, slave, pdu)) = parsed else {
            service.shared.send_event(SlaveEvent::Log {
                dir: "ERR",
                text: "Discarded malformed UDP Modbus frame".into(),
            });
            continue;
        };
        let function = pdu.first().copied().unwrap_or(0);
        let result = match decode_server_request_pdu(pdu) {
            Ok(request) => service
                .call(SlaveRequest { slave, request })
                .await
                .transpose(),
            Err(_) if rtu && slave == 0 => None,
            Err(exception) => Some(Err(exception)),
        };
        let Some(result) = result else {
            continue;
        };
        let response_pdu = encode_server_response_pdu(function, result);
        let mut response = Vec::with_capacity(response_pdu.len() + 7);
        if let Some(transaction) = transaction {
            response.extend_from_slice(&transaction);
            response.extend_from_slice(&[0, 0]);
            response.extend_from_slice(&((response_pdu.len() + 1) as u16).to_be_bytes());
            response.push(slave);
            response.extend_from_slice(&response_pdu);
        } else {
            response.push(slave);
            response.extend_from_slice(&response_pdu);
            response.extend_from_slice(&modbus_crc16(&response).to_le_bytes());
        }
        socket.send_to(&response, peer).await?;
        let _ = traffic.send((true, response));
    }
}

pub(super) fn spawn_slave(cfg: SlaveCfg, ui: UiSink) -> SlaveHandle {
    // 有界队列 + try_send：高频请求下满则丢弃事件(背压/有损)，避免无界内存增长。
    let (events_tx, mut events_rx) = mpsc::channel::<SlaveEvent>(1024);
    let (ctrl_tx, mut ctrl_rx) = mpsc::channel::<SlaveMsg>(ENGINE_CONTROL_QUEUE_CAPACITY);
    let shared = Arc::new(SlaveShared {
        store: Mutex::new(DataStore::new()),
        requests: AtomicU64::new(0),
        events: events_tx,
        event_drops: AtomicU64::new(0),
        event_high_watermark: AtomicUsize::new(0),
    });

    let (traffic_tx, mut traffic_rx) = traffic_channel();
    let slave_is_tcp = matches!(cfg.transport, Transport::Tcp { .. } | Transport::Udp { .. });

    // --- server task ---
    let server_shared = shared.clone();
    let ui_srv = ui.clone();
    let transport = cfg.transport.clone();
    let unit_id = cfg.unit_id;
    let ignore_unit_id = cfg.ignore_unit_id;
    let english = cfg.english;
    let tls = cfg.tls.clone();
    let server = tokio::spawn(async move {
        match transport {
            Transport::Tcp { host, port } => {
                let listener = match TcpListener::bind((host.as_str(), port)).await {
                    Ok(l) => l,
                    Err(e) => {
                        ui_srv.slave_status(bind_error_message(&host, port, &e, english), false);
                        return;
                    }
                };
                let local = listener
                    .local_addr()
                    .map(|a| a.to_string())
                    .unwrap_or_else(|_| format!("{host}:{port}"));
                let svc = SlaveService {
                    shared: server_shared.clone(),
                    unit_id,
                    tcp: true,
                    ignore_unit_id,
                };
                let on_error = |e: std::io::Error| eprintln!("modbus tcp server error: {e}");
                // Each accepted stream is wrapped in a Tap so the DECRYPTED raw ADU
                // bytes reach the Communication Traffic monitor (TLS handled below it).
                if let Some(tcfg) = &tls {
                    let acceptor = match crate::tls::server_acceptor(tcfg) {
                        Ok(a) => a,
                        Err(e) => {
                            ui_srv.slave_status(format!("TLS setup failed: {e}"), false);
                            return;
                        }
                    };
                    ui_srv.slave_status(format!("Listening on {local} (TLS)"), true);
                    let on_connected =
                        move |stream: tokio::net::TcpStream, _peer: std::net::SocketAddr| {
                            let svc = svc.clone();
                            let ttx = traffic_tx.clone();
                            let acceptor = acceptor.clone();
                            async move {
                                let tls_stream = acceptor.accept(stream).await?;
                                Ok::<_, std::io::Error>(Some((
                                    svc.clone(),
                                    Tap {
                                        inner: tls_stream,
                                        tx: ttx,
                                    },
                                )))
                            }
                        };
                    let srv = tokio_modbus::server::tcp::Server::new(listener);
                    if let Err(e) = srv.serve(&on_connected, on_error).await {
                        ui_srv.slave_status(format!("Server stopped: {e}"), false);
                    }
                } else {
                    ui_srv.slave_status(format!("Listening on {local}"), true);
                    let on_connected =
                        move |stream: tokio::net::TcpStream, peer: std::net::SocketAddr| {
                            let svc = svc.clone();
                            let ttx = traffic_tx.clone();
                            async move {
                                let accepted = tokio_modbus::server::tcp::accept_tcp_connection(
                                    stream,
                                    peer,
                                    move |_addr| Ok(Some(svc.clone())),
                                )?;
                                Ok(accepted
                                    .map(|(service, s)| (service, Tap { inner: s, tx: ttx })))
                            }
                        };
                    let srv = tokio_modbus::server::tcp::Server::new(listener);
                    if let Err(e) = srv.serve(&on_connected, on_error).await {
                        ui_srv.slave_status(format!("Server stopped: {e}"), false);
                    }
                }
            }
            Transport::Udp { host, port } => {
                let socket = match tokio::net::UdpSocket::bind((host.as_str(), port)).await {
                    Ok(socket) => socket,
                    Err(error) => {
                        ui_srv
                            .slave_status(bind_error_message(&host, port, &error, english), false);
                        return;
                    }
                };
                let local = socket
                    .local_addr()
                    .map(|address| address.to_string())
                    .unwrap_or_else(|_| format!("{host}:{port}"));
                ui_srv.slave_status(format!("Listening on {local} (Modbus UDP)"), true);
                let service = SlaveService {
                    shared: server_shared.clone(),
                    unit_id,
                    tcp: true,
                    ignore_unit_id,
                };
                if let Err(error) = serve_udp_slave(socket, service, traffic_tx, false).await {
                    ui_srv.slave_status(format!("UDP server stopped: {error}"), false);
                }
            }
            Transport::RtuOverTcp { host, port } => {
                let listener = match TcpListener::bind((host.as_str(), port)).await {
                    Ok(listener) => listener,
                    Err(error) => {
                        ui_srv
                            .slave_status(bind_error_message(&host, port, &error, english), false);
                        return;
                    }
                };
                let local = listener
                    .local_addr()
                    .map(|address| address.to_string())
                    .unwrap_or_else(|_| format!("{host}:{port}"));
                ui_srv.slave_status(format!("Listening on {local} (RTU over TCP)"), true);
                let service = SlaveService {
                    shared: server_shared.clone(),
                    unit_id,
                    tcp: false,
                    ignore_unit_id: false,
                };
                let on_connected =
                    move |stream: tokio::net::TcpStream, peer: std::net::SocketAddr| {
                        let service = service.clone();
                        let traffic = traffic_tx.clone();
                        async move {
                            tokio_modbus::server::rtu_over_tcp::accept_tcp_connection(
                                stream,
                                peer,
                                move |_address| Ok(Some(service.clone())),
                            )
                            .map(|accepted| {
                                accepted.map(|(service, stream)| {
                                    (
                                        service,
                                        Tap {
                                            inner: stream,
                                            tx: traffic,
                                        },
                                    )
                                })
                            })
                        }
                    };
                let on_error =
                    |error: std::io::Error| eprintln!("modbus rtu-over-tcp server error: {error}");
                let server = tokio_modbus::server::rtu_over_tcp::Server::new(listener);
                if let Err(error) = server.serve(&on_connected, on_error).await {
                    ui_srv.slave_status(format!("RTU/TCP server stopped: {error}"), false);
                }
            }
            Transport::RtuOverUdp { host, port } => {
                let socket = match tokio::net::UdpSocket::bind((host.as_str(), port)).await {
                    Ok(socket) => socket,
                    Err(error) => {
                        ui_srv
                            .slave_status(bind_error_message(&host, port, &error, english), false);
                        return;
                    }
                };
                let local = socket
                    .local_addr()
                    .map(|address| address.to_string())
                    .unwrap_or_else(|_| format!("{host}:{port}"));
                ui_srv.slave_status(format!("Listening on {local} (RTU over UDP)"), true);
                let service = SlaveService {
                    shared: server_shared.clone(),
                    unit_id,
                    tcp: false,
                    ignore_unit_id: false,
                };
                if let Err(error) = serve_udp_slave(socket, service, traffic_tx, true).await {
                    ui_srv.slave_status(format!("RTU/UDP server stopped: {error}"), false);
                }
            }
            Transport::Rtu {
                path,
                baud,
                data_bits,
                parity,
                stop_bits,
            } => {
                drop(traffic_tx); // the RTU server owns a concrete SerialStream — cannot be tapped
                let serial = match SerialStream::open(&serial_builder(
                    &path, baud, data_bits, parity, stop_bits,
                )) {
                    Ok(s) => s,
                    Err(e) => {
                        ui_srv.slave_status(format!("Open failed: {e}"), false);
                        return;
                    }
                };
                ui_srv.slave_status(format!("Serving on {path}"), true);
                let svc = SlaveService {
                    shared: server_shared.clone(),
                    unit_id,
                    tcp: false,
                    ignore_unit_id,
                };
                let srv = tokio_modbus::server::rtu::Server::new(serial);
                if let Err(e) = srv.serve_forever(svc).await {
                    ui_srv.slave_status(format!("Server stopped: {e}"), false);
                }
            }
        }
    });

    // --- updater task (render, log, traffic, simulation, display settings) ---
    let upd_shared = shared.clone();
    let view0 = SlaveView {
        area: cfg.area,
        address: cfg.address,
        quantity: cfg.quantity,
        format: cfg.format,
    };
    let handle_ui = ui.clone();
    let updater = tokio::spawn(async move {
        let mut view = view0;
        let mut names: HashMap<u16, String> = HashMap::new();
        let mut scaling = Scaling::off();
        let mut colors = ColorRules::off();
        let mut value_names = ValueNames::off();
        let mut sim: Option<SimCfg> = None;
        let mut rng = Xorshift::new(rng_seed());
        let mut chart = ChartState::new();
        let mut logbuf: Vec<crate::LogLine> = Vec::new();
        let mut trafficbuf: Vec<crate::LogLine> = Vec::new();
        let mut last_names: Vec<slint::SharedString> = Vec::new();
        let mut sim_tick = tokio::time::interval(Duration::from_secs(3600));
        let mut traffic_health_tick = tokio::time::interval(Duration::from_secs(1));
        let mut traffic_flush_tick = tokio::time::interval(Duration::from_millis(50));
        let mut log_flush_tick = tokio::time::interval(Duration::from_millis(50));
        let mut reported_traffic_drops = 0;
        let mut reported_event_drops = 0;
        let mut traffic_dirty = false;
        let mut log_dirty = false;
        // Per-register auto-increment toggled from the row context menu.
        let mut auto_inc: std::collections::HashSet<(Area, u16)> = std::collections::HashSet::new();
        let mut ainc_tick = tokio::time::interval(Duration::from_millis(500));
        // Per-cell display-format overrides (Set format… 右键菜单), 与主站对等。
        let mut cell_formats: HashMap<u16, RegFormat> = HashMap::new();
        render_slave(
            &upd_shared,
            &view,
            &names,
            &scaling,
            &colors,
            &value_names,
            &mut chart,
            &mut last_names,
            &auto_inc,
            &cell_formats,
            &ui,
        );
        loop {
            tokio::select! {
                Some(ev) = events_rx.recv() => {
                    ui.slave_requests(upd_shared.requests.load(Ordering::Relaxed));
                    match ev {
                        SlaveEvent::Changed => {
                            render_slave(&upd_shared, &view, &names, &scaling, &colors, &value_names, &mut chart, &mut last_names, &auto_inc, &cell_formats, &ui);
                        }
                        SlaveEvent::Log { dir, text } => {
                            logbuf.push(crate::LogLine {
                                time: now_hms().into(),
                                dir: dir.into(),
                                text: text.into(),
                            });
                            if logbuf.len() > 300 {
                                let excess = logbuf.len() - 300;
                                logbuf.drain(0..excess);
                            }
                            log_dirty = true;
                        },
                    }
                }
                Some((is_tx, bytes)) = traffic_rx.rx.recv() => {
                    let hex = bytes.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(" ");
                    let text = match parse_modbus_adu(is_tx, &bytes, slave_is_tcp) {
                        Some(p) => format!("{p}   │   {hex}"),
                        None => hex,
                    };
                    trafficbuf.push(crate::LogLine {
                        time: now_hms().into(),
                        dir: if is_tx { "Tx" } else { "Rx" }.into(),
                        text: text.into(),
                    });
                    if trafficbuf.len() > 500 {
                        let excess = trafficbuf.len() - 500;
                        trafficbuf.drain(0..excess);
                    }
                    traffic_dirty = true;
                }
                _ = traffic_flush_tick.tick(), if traffic_dirty => {
                    ui.slave_traffic(trafficbuf.clone());
                    traffic_dirty = false;
                }
                _ = log_flush_tick.tick(), if log_dirty => {
                    ui.slave_log(logbuf.clone());
                    log_dirty = false;
                }
                _ = traffic_health_tick.tick() => {
                    let dropped = traffic_rx.health.dropped_chunks.load(Ordering::Relaxed);
                    if dropped > reported_traffic_drops {
                        let bytes = traffic_rx.health.dropped_bytes.load(Ordering::Relaxed);
                        let high = traffic_rx.health.high_watermark.load(Ordering::Relaxed);
                        trafficbuf.push(crate::LogLine {
                            time: now_hms().into(),
                            dir: "ERR".into(),
                            text: format!(
                                "Traffic monitor queue overflow: dropped {dropped} chunks / {bytes} bytes, H{high}/{TRAFFIC_QUEUE_CAPACITY}"
                            ).into(),
                        });
                        if trafficbuf.len() > 500 {
                            let excess = trafficbuf.len() - 500;
                            trafficbuf.drain(0..excess);
                        }
                        traffic_dirty = true;
                        reported_traffic_drops = dropped;
                    }
                    let event_drops = upd_shared.event_drops.load(Ordering::Relaxed);
                    if event_drops > reported_event_drops {
                        let high = upd_shared.event_high_watermark.load(Ordering::Relaxed);
                        ui.slave_message(format!(
                            "Slave event queue overflow: dropped {event_drops}, H{high}/1024"
                        ));
                        logbuf.push(crate::LogLine {
                            time: now_hms().into(),
                            dir: "ERR".into(),
                            text: format!(
                                "Slave event queue overflow: dropped {event_drops}, H{high}/1024"
                            ).into(),
                        });
                        if logbuf.len() > 300 {
                            let excess = logbuf.len() - 300;
                            logbuf.drain(0..excess);
                        }
                        log_dirty = true;
                        reported_event_drops = event_drops;
                    }
                }
                _ = sim_tick.tick(), if sim.is_some() => {
                    if let Some(cfg) = &sim {
                        {
                            let mut store = upd_shared.store.lock().unwrap();
                            apply_sim(&mut store, cfg, &mut rng);
                        }
                        render_slave(&upd_shared, &view, &names, &scaling, &colors, &value_names, &mut chart, &mut last_names, &auto_inc, &cell_formats, &ui);
                    }
                }
                _ = ainc_tick.tick(), if !auto_inc.is_empty() => {
                    {
                        let mut store = upd_shared.store.lock().unwrap();
                        for (area, addr) in &auto_inc {
                            bump_one(&mut store, *area, *addr);
                        }
                    }
                    render_slave(&upd_shared, &view, &names, &scaling, &colors, &value_names, &mut chart, &mut last_names, &auto_inc, &cell_formats, &ui);
                }
                msg = ctrl_rx.recv() => {
                    match msg {
                        Some(SlaveMsg::SetView { area, address, quantity, format }) => {
                            view = SlaveView { area, address, quantity, format };
                            chart.reset();
                            render_slave(&upd_shared, &view, &names, &scaling, &colors, &value_names, &mut chart, &mut last_names, &auto_inc, &cell_formats, &ui);
                        }
                        Some(SlaveMsg::Edit { address, text }) => {
                            apply_edit(&upd_shared, view.area, address, &text);
                            render_slave(&upd_shared, &view, &names, &scaling, &colors, &value_names, &mut chart, &mut last_names, &auto_inc, &cell_formats, &ui);
                        }
                        Some(SlaveMsg::EditAt { area, address, text }) => {
                            // 导入按 Function 列写到指定表(与当前视图无关)
                            apply_edit(&upd_shared, area, address, &text);
                            render_slave(&upd_shared, &view, &names, &scaling, &colors, &value_names, &mut chart, &mut last_names, &auto_inc, &cell_formats, &ui);
                        }
                        Some(SlaveMsg::SetName { address, name }) => {
                            if name.trim().is_empty() { names.remove(&address); } else { names.insert(address, name); }
                            render_slave(&upd_shared, &view, &names, &scaling, &colors, &value_names, &mut chart, &mut last_names, &auto_inc, &cell_formats, &ui);
                        }
                        Some(SlaveMsg::SetCellFormat { address, format }) => {
                            match format { Some(f) => { cell_formats.insert(address, f); }, None => { cell_formats.remove(&address); } }
                            render_slave(&upd_shared, &view, &names, &scaling, &colors, &value_names, &mut chart, &mut last_names, &auto_inc, &cell_formats, &ui);
                        }
                        Some(SlaveMsg::ExportCsv(path)) => {
                            let store = upd_shared.store.lock().unwrap();
                            let out = build_server_csv(&store, &names);
                            drop(store);
                            let result_ui = ui.clone();
                            tokio::task::spawn_blocking(move || {
                                let result = std::fs::write(&path, out);
                                result_ui.file_result(true, "CSV 导出", path, result);
                            });
                        }
                        Some(SlaveMsg::SetScaling(s)) => {
                            scaling = s;
                            chart.reset();
                            render_slave(&upd_shared, &view, &names, &scaling, &colors, &value_names, &mut chart, &mut last_names, &auto_inc, &cell_formats, &ui);
                        }
                        Some(SlaveMsg::SetColors(c)) => {
                            colors = c;
                            render_slave(&upd_shared, &view, &names, &scaling, &colors, &value_names, &mut chart, &mut last_names, &auto_inc, &cell_formats, &ui);
                        }
                        Some(SlaveMsg::SetValueNames(v)) => {
                            value_names = v;
                            render_slave(&upd_shared, &view, &names, &scaling, &colors, &value_names, &mut chart, &mut last_names, &auto_inc, &cell_formats, &ui);
                        }
                        Some(SlaveMsg::SimStart(c)) => {
                            sim_tick = tokio::time::interval(Duration::from_millis(c.interval_ms.max(50)));
                            sim = Some(c);
                        }
                        Some(SlaveMsg::SimStop) => sim = None,
                        Some(SlaveMsg::ToggleAutoInc { address }) => {
                            let key = (view.area, address);
                            if !auto_inc.remove(&key) {
                                auto_inc.insert(key);
                            }
                            render_slave(&upd_shared, &view, &names, &scaling, &colors, &value_names, &mut chart, &mut last_names, &auto_inc, &cell_formats, &ui);
                        }
                        None => break,
                    }
                }
                else => break,
            }
        }
    });

    SlaveHandle {
        ctrl: ctrl_tx,
        server,
        updater,
        ui: handle_ui,
    }
}

// ----- simulation -----

pub(super) struct Xorshift(u64);

impl Xorshift {
    pub(super) fn new(seed: u64) -> Self {
        Xorshift(seed | 1)
    }
    pub(super) fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
}

pub(super) fn rng_seed() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9E37_79B9_7F4A_7C15)
        | 1
}

/// Step one value for the client-side local simulator.
pub(super) fn step_value(
    cur: u16,
    mode: SimMode,
    step: u16,
    min: i64,
    max: i64,
    rng: &mut Xorshift,
) -> u16 {
    // 容忍 min>max 的非法配置：先归一，避免 span=1 导致仿真值卡死。
    let (min, max) = (min.min(max), min.max(max));
    let cur = cur as i64;
    let span = (max - min) as u64 + 1;
    let next = match mode {
        SimMode::Increment => {
            let n = cur + step as i64;
            if n > max {
                min
            } else {
                n
            }
        }
        SimMode::Decrement => {
            let n = cur - step as i64;
            if n < min {
                max
            } else {
                n
            }
        }
        SimMode::Random => min + (rng.next() % span) as i64,
        SimMode::Toggle => {
            if cur != min {
                min
            } else {
                max
            }
        }
        SimMode::Off => cur,
    };
    next as u16
}

/// Per-register auto-increment (right-click toggle): +1 (wrapping) for registers,
/// flip for bits, on a single address in the given area.
pub(super) fn bump_one(store: &mut DataStore, area: Area, addr: u16) {
    let i = addr as usize;
    match area {
        Area::Coils => {
            if let Some(b) = store.coils.get_mut(i) {
                *b = !*b;
            }
        }
        Area::DiscreteInputs => {
            if let Some(b) = store.discrete_inputs.get_mut(i) {
                *b = !*b;
            }
        }
        Area::HoldingRegisters => {
            if let Some(v) = store.holding.get_mut(i) {
                *v = v.wrapping_add(1);
            }
        }
        Area::InputRegisters => {
            if let Some(v) = store.input.get_mut(i) {
                *v = v.wrapping_add(1);
            }
        }
    }
}

pub(super) fn apply_sim(store: &mut DataStore, cfg: &SimCfg, rng: &mut Xorshift) {
    let lo = cfg.address as usize;
    let hi = lo + cfg.quantity as usize;
    match cfg.area {
        Area::HoldingRegisters => sim_regs(&mut store.holding, lo, hi, cfg, rng),
        Area::InputRegisters => sim_regs(&mut store.input, lo, hi, cfg, rng),
        Area::Coils => sim_bits(&mut store.coils, lo, hi, cfg, rng),
        Area::DiscreteInputs => sim_bits(&mut store.discrete_inputs, lo, hi, cfg, rng),
    }
}

/// Whether absolute index `i` (within [lo,hi)) should be animated this tick,
/// honouring `cfg.target` (-1 = all; else only that single absolute address).
pub(super) fn sim_hits(cfg: &SimCfg, i: usize) -> bool {
    cfg.target < 0 || cfg.target as usize == i
}

pub(super) fn sim_regs(mem: &mut [u16], lo: usize, hi: usize, cfg: &SimCfg, rng: &mut Xorshift) {
    let hi = hi.min(mem.len());
    for (i, value) in mem.iter_mut().enumerate().take(hi).skip(lo) {
        if !sim_hits(cfg, i) {
            continue;
        }
        *value = step_value(*value, cfg.mode, cfg.step, cfg.min, cfg.max, rng);
    }
}

pub(super) fn sim_bits(mem: &mut [bool], lo: usize, hi: usize, cfg: &SimCfg, rng: &mut Xorshift) {
    let hi = hi.min(mem.len());
    for (i, value) in mem.iter_mut().enumerate().take(hi).skip(lo) {
        if !sim_hits(cfg, i) {
            continue;
        }
        *value = match cfg.mode {
            SimMode::Random => rng.next() & 1 == 1,
            SimMode::Off => *value,
            _ => !*value, // increment/decrement/toggle all flip a bit
        };
    }
}

pub(super) fn handle_request(
    shared: &SlaveShared,
    req: &Request,
) -> Result<Response, ExceptionCode> {
    let mut store = shared.store.lock().unwrap();
    match req {
        Request::ReadCoils(a, c) => read_bits(&store.coils, *a, *c).map(Response::ReadCoils),
        Request::ReadDiscreteInputs(a, c) => {
            read_bits(&store.discrete_inputs, *a, *c).map(Response::ReadDiscreteInputs)
        }
        Request::ReadHoldingRegisters(a, c) => {
            read_words(&store.holding, *a, *c).map(Response::ReadHoldingRegisters)
        }
        Request::ReadInputRegisters(a, c) => {
            read_words(&store.input, *a, *c).map(Response::ReadInputRegisters)
        }
        Request::WriteSingleCoil(a, v) => {
            put_bit(&mut store.coils, *a, *v)?;
            Ok(Response::WriteSingleCoil(*a, *v))
        }
        Request::WriteSingleRegister(a, v) => {
            put_word(&mut store.holding, *a, *v)?;
            Ok(Response::WriteSingleRegister(*a, *v))
        }
        Request::WriteMultipleCoils(a, vs) => {
            put_bits(&mut store.coils, *a, &vs[..])?;
            Ok(Response::WriteMultipleCoils(*a, vs.len() as u16))
        }
        Request::WriteMultipleRegisters(a, vs) => {
            put_words(&mut store.holding, *a, &vs[..])?;
            Ok(Response::WriteMultipleRegisters(*a, vs.len() as u16))
        }
        Request::MaskWriteRegister(a, and_mask, or_mask) => {
            let i = *a as usize;
            if i >= store.holding.len() {
                return Err(ExceptionCode::IllegalDataAddress);
            }
            // result = (current AND and_mask) OR (or_mask AND (NOT and_mask))
            store.holding[i] = (store.holding[i] & and_mask) | (or_mask & !and_mask);
            Ok(Response::MaskWriteRegister(*a, *and_mask, *or_mask))
        }
        Request::ReadWriteMultipleRegisters(read_addr, read_qty, write_addr, vs) => {
            put_words(&mut store.holding, *write_addr, &vs[..])?;
            let r = read_words(&store.holding, *read_addr, *read_qty)?;
            Ok(Response::ReadWriteMultipleRegisters(r))
        }
        _ => Err(ExceptionCode::IllegalFunction),
    }
}

pub(super) fn is_write(req: &Request) -> bool {
    matches!(
        req,
        Request::WriteSingleCoil(..)
            | Request::WriteSingleRegister(..)
            | Request::WriteMultipleCoils(..)
            | Request::WriteMultipleRegisters(..)
            | Request::MaskWriteRegister(..)
            | Request::ReadWriteMultipleRegisters(..)
    )
}

pub(super) fn read_bits(mem: &[bool], addr: u16, cnt: u16) -> Result<Vec<bool>, ExceptionCode> {
    let s = addr as usize;
    let e = s + cnt as usize;
    if e > mem.len() {
        return Err(ExceptionCode::IllegalDataAddress);
    }
    Ok(mem[s..e].to_vec())
}

pub(super) fn read_words(mem: &[u16], addr: u16, cnt: u16) -> Result<Vec<u16>, ExceptionCode> {
    let s = addr as usize;
    let e = s + cnt as usize;
    if e > mem.len() {
        return Err(ExceptionCode::IllegalDataAddress);
    }
    Ok(mem[s..e].to_vec())
}

pub(super) fn put_bit(mem: &mut [bool], addr: u16, v: bool) -> Result<(), ExceptionCode> {
    let i = addr as usize;
    if i >= mem.len() {
        return Err(ExceptionCode::IllegalDataAddress);
    }
    mem[i] = v;
    Ok(())
}

pub(super) fn put_word(mem: &mut [u16], addr: u16, v: u16) -> Result<(), ExceptionCode> {
    let i = addr as usize;
    if i >= mem.len() {
        return Err(ExceptionCode::IllegalDataAddress);
    }
    mem[i] = v;
    Ok(())
}

pub(super) fn put_bits(mem: &mut [bool], addr: u16, vs: &[bool]) -> Result<(), ExceptionCode> {
    let b = addr as usize;
    if b + vs.len() > mem.len() {
        return Err(ExceptionCode::IllegalDataAddress);
    }
    mem[b..b + vs.len()].copy_from_slice(vs);
    Ok(())
}

pub(super) fn put_words(mem: &mut [u16], addr: u16, vs: &[u16]) -> Result<(), ExceptionCode> {
    let b = addr as usize;
    if b + vs.len() > mem.len() {
        return Err(ExceptionCode::IllegalDataAddress);
    }
    mem[b..b + vs.len()].copy_from_slice(vs);
    Ok(())
}

pub(super) fn describe_request(req: &Request) -> String {
    match req {
        Request::ReadCoils(a, c) => format!("Read Coils @{a} ×{c}"),
        Request::ReadDiscreteInputs(a, c) => format!("Read Discrete Inputs @{a} ×{c}"),
        Request::ReadHoldingRegisters(a, c) => format!("Read Holding Registers @{a} ×{c}"),
        Request::ReadInputRegisters(a, c) => format!("Read Input Registers @{a} ×{c}"),
        Request::WriteSingleCoil(a, v) => format!("Write Coil @{a} = {}", if *v { 1 } else { 0 }),
        Request::WriteSingleRegister(a, v) => format!("Write Register @{a} = {v}"),
        Request::WriteMultipleCoils(a, vs) => format!("Write {} Coils @{a}", vs.len()),
        Request::WriteMultipleRegisters(a, vs) => format!("Write {} Registers @{a}", vs.len()),
        other => format!("{other:?}"),
    }
}

/// 服务端"全部导出": 扫 4 表, 只导"有名字 或 值非零"的寄存器, 每行标功能码(表)。
pub(super) fn build_server_csv(store: &DataStore, names: &HashMap<u16, String>) -> String {
    let mut out = String::from("Import,Function,Address,Name,Value\n");
    let nm = |i: usize| names.get(&(i as u16)).map(|s| s.as_str()).unwrap_or("");
    for (i, &b) in store.coils.iter().enumerate() {
        if b || names.contains_key(&(i as u16)) {
            out.push_str(&format!(
                "yes,{},{},{},{}\n",
                Area::Coils.csv_name(),
                i,
                csv_field(nm(i)),
                if b { 1 } else { 0 }
            ));
        }
    }
    for (i, &b) in store.discrete_inputs.iter().enumerate() {
        if b || names.contains_key(&(i as u16)) {
            out.push_str(&format!(
                "yes,{},{},{},{}\n",
                Area::DiscreteInputs.csv_name(),
                i,
                csv_field(nm(i)),
                if b { 1 } else { 0 }
            ));
        }
    }
    for (i, &v) in store.holding.iter().enumerate() {
        if v != 0 || names.contains_key(&(i as u16)) {
            out.push_str(&format!(
                "yes,{},{},{},{}\n",
                Area::HoldingRegisters.csv_name(),
                i,
                csv_field(nm(i)),
                v
            ));
        }
    }
    for (i, &v) in store.input.iter().enumerate() {
        if v != 0 || names.contains_key(&(i as u16)) {
            out.push_str(&format!(
                "yes,{},{},{},{}\n",
                Area::InputRegisters.csv_name(),
                i,
                csv_field(nm(i)),
                v
            ));
        }
    }
    out
}

pub(super) fn apply_edit(shared: &SlaveShared, area: Area, address: u16, text: &str) {
    let mut store = shared.store.lock().unwrap();
    let idx = address as usize;
    match area {
        Area::Coils => {
            if let Some(b) = parse_bit(text) {
                if idx < store.coils.len() {
                    store.coils[idx] = b;
                }
            }
        }
        Area::DiscreteInputs => {
            if let Some(b) = parse_bit(text) {
                if idx < store.discrete_inputs.len() {
                    store.discrete_inputs[idx] = b;
                }
            }
        }
        Area::HoldingRegisters => {
            if let Some(v) = parse_word(text) {
                if idx < store.holding.len() {
                    store.holding[idx] = v;
                }
            }
        }
        Area::InputRegisters => {
            if let Some(v) = parse_word(text) {
                if idx < store.input.len() {
                    store.input[idx] = v;
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn render_slave(
    shared: &SlaveShared,
    view: &SlaveView,
    names: &HashMap<u16, String>,
    scaling: &Scaling,
    colors: &ColorRules,
    value_names: &ValueNames,
    chart: &mut ChartState,
    last_names: &mut Vec<slint::SharedString>,
    auto_inc: &std::collections::HashSet<(Area, u16)>,
    cell_formats: &HashMap<u16, RegFormat>,
    ui: &UiSink,
) {
    let store = shared.store.lock().unwrap();
    let start = view.address as usize;
    let qty = view.quantity as usize;
    let rows = match view.area {
        Area::Coils => render_bits(view.address, window_bool(&store.coils, start, qty)),
        Area::DiscreteInputs => render_bits(
            view.address,
            window_bool(&store.discrete_inputs, start, qty),
        ),
        Area::HoldingRegisters => render_registers(
            view.address,
            window_u16(&store.holding, start, qty),
            view.format,
            cell_formats,
            scaling,
        ),
        Area::InputRegisters => render_registers(
            view.address,
            window_u16(&store.input, start, qty),
            view.format,
            cell_formats,
            scaling,
        ),
    };
    drop(store);
    update_chart(chart, &rows);
    let (mut rr, nn) = build_grid(rows, names, colors, value_names, true, view.area);
    for r in rr.iter_mut() {
        r.auto_inc = auto_inc.contains(&(view.area, r.address as u16));
    }
    ui.slave_rows(rr);
    // Push the names model only when it changes, so editing isn't clobbered.
    if nn != *last_names {
        *last_names = nn.clone();
        ui.slave_names(nn);
    }
    let (charts, has) = build_charts(chart);
    ui.slave_chart(charts, has);
}

/// Render one floating server monitor's view from the shared store and push the
/// rows to its window. Monitors are plain views (no names/colors/value-names).
pub(super) fn window_bool(mem: &[bool], start: usize, qty: usize) -> &[bool] {
    if start >= mem.len() {
        return &[];
    }
    &mem[start..(start + qty).min(mem.len())]
}

pub(super) fn window_u16(mem: &[u16], start: usize, qty: usize) -> &[u16] {
    if start >= mem.len() {
        return &[];
    }
    &mem[start..(start + qty).min(mem.len())]
}

// ===========================================================================
// Serial helper
// ===========================================================================
