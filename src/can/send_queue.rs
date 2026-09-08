//! Bounded, time-sliced CAN send scheduling.
use super::*;

pub(super) const MAX_PENDING_SEND_JOBS: usize = 64;
pub(super) const MAX_PENDING_SEND_FRAMES: u64 = 100_000;
pub(super) const SEND_FRAMES_PER_SLICE: usize = 32;
pub(super) const SEND_SLICE_BUDGET: Duration = Duration::from_millis(2);

pub(super) enum PendingSendSource {
    Sequence {
        next: CanFrame,
        id_increment: bool,
        data_increment: bool,
    },
    Batch {
        frames: Vec<CanFrame>,
        index: usize,
    },
}

pub(super) struct PendingSendJob {
    pub(super) source: PendingSendSource,
    pub(super) total: u64,
    pub(super) emitted: u64,
}

impl PendingSendJob {
    pub(super) fn sequence(
        frame: CanFrame,
        count: u64,
        id_increment: bool,
        data_increment: bool,
    ) -> Option<Self> {
        (count > 0).then_some(Self {
            source: PendingSendSource::Sequence {
                next: frame,
                id_increment,
                data_increment,
            },
            total: count.min(MAX_PENDING_SEND_FRAMES),
            emitted: 0,
        })
    }

    pub(super) fn batch(frames: Vec<CanFrame>, repeat: u32) -> Option<Self> {
        if frames.is_empty() || repeat == 0 {
            return None;
        }
        let total = (frames.len() as u64)
            .saturating_mul(repeat as u64)
            .min(MAX_PENDING_SEND_FRAMES);
        Some(Self {
            source: PendingSendSource::Batch { frames, index: 0 },
            total,
            emitted: 0,
        })
    }

    pub(super) fn remaining(&self) -> u64 {
        self.total.saturating_sub(self.emitted)
    }

    pub(super) fn next_frame(&mut self) -> Option<CanFrame> {
        if self.emitted >= self.total {
            return None;
        }
        let frame = match &mut self.source {
            PendingSendSource::Sequence {
                next,
                id_increment,
                data_increment,
            } => {
                let frame = next.clone();
                if *id_increment {
                    let id_mask = if next.ext { 0x1FFF_FFFF } else { 0x7FF };
                    next.id = next.id.wrapping_add(1) & id_mask;
                }
                if *data_increment {
                    increment_frame_data(&mut next.data);
                }
                frame
            }
            PendingSendSource::Batch { frames, index } => {
                let frame = frames[*index].clone();
                *index = (*index + 1) % frames.len();
                frame
            }
        };
        self.emitted += 1;
        Some(frame)
    }
}

pub(super) fn increment_frame_data(data: &mut [u8]) {
    for byte in data {
        let (value, carry) = byte.overflowing_add(1);
        *byte = value;
        if !carry {
            break;
        }
    }
}

pub(super) fn pending_send_frames(queue: &VecDeque<PendingSendJob>) -> u64 {
    queue.iter().map(PendingSendJob::remaining).sum()
}

pub(super) fn enqueue_send_job(
    queue: &mut VecDeque<PendingSendJob>,
    job: PendingSendJob,
) -> Result<u64, &'static str> {
    if queue.len() >= MAX_PENDING_SEND_JOBS {
        return Err("发送任务过多，请等待当前任务完成");
    }
    let remaining = job.remaining();
    if pending_send_frames(queue).saturating_add(remaining) > MAX_PENDING_SEND_FRAMES {
        return Err("待发送帧已达到 100000 帧安全上限");
    }
    queue.push_back(job);
    Ok(remaining)
}

pub(super) fn process_pending_sends(
    queue: &mut VecDeque<PendingSendJob>,
    adapters: &mut Vec<(u8, Box<dyn CanAdapter>)>,
    evt_tx: &EventSender,
    start: Instant,
) {
    if queue.is_empty() || adapters.is_empty() {
        return;
    }
    let deadline = Instant::now() + SEND_SLICE_BUDGET;
    let mut echoes = Vec::with_capacity(SEND_FRAMES_PER_SLICE);
    for _ in 0..SEND_FRAMES_PER_SLICE {
        if Instant::now() >= deadline {
            break;
        }
        let Some(job) = queue.front_mut() else {
            break;
        };
        let Some(mut frame) = job.next_frame() else {
            queue.pop_front();
            continue;
        };
        let job_complete = job.remaining() == 0;
        frame.t = start.elapsed().as_secs_f64();
        frame.tx = true;
        match send_on(adapters, &frame) {
            Ok(channel) => {
                frame.ch = channel;
                echoes.push(frame);
                if job_complete {
                    queue.pop_front();
                }
            }
            Err(error) => {
                queue.pop_front();
                let _ = evt_tx.send(Evt::Log(format!("发送任务已停止: {error}")));
                break;
            }
        }
    }
    if !echoes.is_empty() {
        let _ = evt_tx.send(Evt::Frames(echoes));
    }
}
