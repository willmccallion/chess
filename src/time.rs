use crate::types::Color;

#[derive(Copy, Clone, Default)]
pub struct TimeControl {
    pub wtime: i64,
    pub btime: i64,
    pub winc: i64,
    pub binc: i64,
    pub movestogo: i32,
    pub move_overhead_ms: i64,
}

impl TimeControl {
    pub fn allocation_ms(&self, turn: Color) -> (u64, u64) {
        let (time, inc) = if turn == Color::White {
            (self.wtime, self.winc)
        } else {
            (self.btime, self.binc)
        };

        if time <= 0 {
            return (10, 50);
        }

        let overhead = self.move_overhead_ms.max(10);
        let time_left = (time - overhead).max(10);

        if self.movestogo > 0 {
            let moves = self.movestogo as i64;
            let time_slice = time_left / (moves + 2);
            let soft = time_slice + inc;
            let hard = time_left.min(soft * 4);
            return (soft as u64, hard as u64);
        }

        let soft = (time_left / 20) + (inc / 2);
        let hard = (time_left / 4).min(soft * 5);

        (soft as u64, hard as u64)
    }
}
