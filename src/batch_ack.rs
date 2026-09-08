//! Bounded, nonblocking batch acknowledgements on the UI timer.
use super::*;
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError};
use std::time::{Duration, Instant};

pub(super) enum Target {
    Ipc(SyncSender<ipc::IpcResp>),
    File {
        window: slint::Weak<TxWindow>,
        path: String,
        english: bool,
    },
}
pub(super) struct Pending {
    receiver: Receiver<Result<u64, String>>,
    deadline: Instant,
    target: Target,
}
pub(super) fn submit(
    a: &mut App,
    frames: Vec<CanFrame>,
    repeat: u32,
    target: Target,
) -> Result<(), String> {
    if a.pending_batches.len() >= 128 {
        return Err("发送确认队列已满，请稍后重试".into());
    }
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    a.cmd
        .send(Cmd::SendBatch {
            frames,
            repeat,
            ack: Some(tx),
        })
        .map_err(|_| "CAN 后台已退出或命令队列已满".to_string())?;
    a.pending_batches.push(Pending {
        receiver: rx,
        deadline: Instant::now() + Duration::from_millis(500),
        target,
    });
    Ok(())
}
#[derive(Debug)]
struct AckError {
    code: &'static str,
    message: String,
}
fn result(
    receiver: &Receiver<Result<u64, String>>,
    deadline: Instant,
    now: Instant,
) -> Option<Result<u64, AckError>> {
    match receiver.try_recv() {
        Ok(value) => Some(value.map_err(|message| AckError {
            code: "QUEUE_REJECTED",
            message,
        })),
        Err(TryRecvError::Disconnected) => Some(Err(AckError {
            code: "TIMEOUT",
            message: "CAN 后台确认通道已关闭，任务状态未知".into(),
        })),
        Err(TryRecvError::Empty) if now >= deadline => Some(Err(AckError {
            code: "TIMEOUT",
            message: "CAN 后台未在 500ms 内确认，任务状态未知；请勿自动重试".into(),
        })),
        Err(TryRecvError::Empty) => None,
    }
}
pub(super) fn poll(a: &mut App) {
    let mut index = 0;
    while index < a.pending_batches.len() {
        let pending = &a.pending_batches[index];
        let Some(outcome) = result(&pending.receiver, pending.deadline, Instant::now()) else {
            index += 1;
            continue;
        };
        match a.pending_batches.remove(index).target {
            Target::Ipc(reply) => {
                let response = match outcome {
                    Ok(queued) => ipc::IpcResp::Ok(serde_json::json!({"queued": queued})),
                    Err(error) => ipc::IpcResp::Err {
                        code: error.code.into(),
                        msg: error.message,
                    },
                };
                let _ = reply.try_send(response);
            }
            Target::File {
                window,
                path,
                english,
            } => {
                if let Some(window) = window.upgrade() {
                    match outcome {
                        Ok(total) => {
                            window.set_tx_file_progress(1.0);
                            window.set_tx_file_status(
                                if english {
                                    format!("Queued {total} frames")
                                } else {
                                    format!("已提交 {total} 帧")
                                }
                                .into(),
                            );
                            a.log(format!("File send queued: {path}, total {total} frames"));
                        }
                        Err(error) => window.set_tx_file_status(error.message.into()),
                    }
                }
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pending_ack_never_waits_and_retains_backend_result() {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let now = Instant::now();
        assert!(result(&rx, now + Duration::from_secs(1), now).is_none());
        tx.send(Ok(17)).unwrap();
        assert_eq!(result(&rx, now, now).unwrap().unwrap(), 17);
        assert!(result(&rx, now, now).unwrap().is_err());
        drop(tx);
        assert!(
            result(&rx, now + Duration::from_secs(1), now)
                .unwrap()
                .is_err()
        );
    }
}
