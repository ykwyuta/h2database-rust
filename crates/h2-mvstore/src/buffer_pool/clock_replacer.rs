use std::sync::atomic::{AtomicBool, Ordering};
use parking_lot::Mutex;

/// Clock-sweep (Second Chance) 置換アルゴリズム
pub struct ClockReplacer {
    num_frames: usize,
    clock_hand: Mutex<usize>,
    ref_bits: Vec<AtomicBool>,
    in_replacer: Vec<AtomicBool>,
}

impl ClockReplacer {
    pub fn new(num_frames: usize) -> Self {
        let mut ref_bits = Vec::with_capacity(num_frames);
        let mut in_replacer = Vec::with_capacity(num_frames);
        for _ in 0..num_frames {
            ref_bits.push(AtomicBool::new(false));
            in_replacer.push(AtomicBool::new(false));
        }
        Self {
            num_frames,
            clock_hand: Mutex::new(0),
            ref_bits,
            in_replacer,
        }
    }

    /// victim (追出し候補) フレーム番号を決定
    pub fn victim(&self) -> Option<usize> {
        let mut hand = self.clock_hand.lock();
        let _start = *hand;

        for _ in 0..(self.num_frames * 2) {
            let curr = *hand;
            *hand = (*hand + 1) % self.num_frames;

            if self.in_replacer[curr].load(Ordering::Relaxed) {
                if self.ref_bits[curr].load(Ordering::Relaxed) {
                    // 参照ビットをクリアしてセカンドチャンスを与える
                    self.ref_bits[curr].store(false, Ordering::Relaxed);
                } else {
                    // victim 決定
                    self.in_replacer[curr].store(false, Ordering::Relaxed);
                    return Some(curr);
                }
            }
        }

        // 全てが参照中か空きがない場合
        None
    }

    /// フレームがピン留めされた（置換対象外にする）
    pub fn pin(&self, frame_id: usize) {
        if frame_id < self.num_frames {
            self.in_replacer[frame_id].store(false, Ordering::Relaxed);
            self.ref_bits[frame_id].store(false, Ordering::Relaxed);
        }
    }

    /// フレームがアンピンされた（pin_count == 0 で置換対象に復帰）
    pub fn unpin(&self, frame_id: usize) {
        if frame_id < self.num_frames {
            self.in_replacer[frame_id].store(true, Ordering::Relaxed);
            self.ref_bits[frame_id].store(true, Ordering::Relaxed);
        }
    }

    pub fn size(&self) -> usize {
        self.in_replacer.iter().filter(|b| b.load(Ordering::Relaxed)).count()
    }
}
