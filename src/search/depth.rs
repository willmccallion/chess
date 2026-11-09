use std::fmt;
use std::ops::{Add, AddAssign, Div, Sub, SubAssign};

/// Plies left to search below a node; at zero or less quiescence takes over.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Depth(i32);

impl Depth {
    pub const ZERO: Self = Self(0);
    pub const ONE: Self = Self(1);

    pub const fn new(plies: i32) -> Self {
        Self(plies)
    }

    pub const fn plies(self) -> i32 {
        self.0
    }
}

impl Add for Depth {
    type Output = Self;

    #[inline(always)]
    fn add(self, other: Self) -> Self {
        Self(self.0 + other.0)
    }
}

impl Sub for Depth {
    type Output = Self;

    #[inline(always)]
    fn sub(self, other: Self) -> Self {
        Self(self.0 - other.0)
    }
}

impl AddAssign for Depth {
    #[inline(always)]
    fn add_assign(&mut self, other: Self) {
        self.0 += other.0;
    }
}

impl SubAssign for Depth {
    #[inline(always)]
    fn sub_assign(&mut self, other: Self) {
        self.0 -= other.0;
    }
}

/// A fraction of the depth, rounded toward zero.
impl Div<i32> for Depth {
    type Output = Self;

    #[inline(always)]
    fn div(self, divisor: i32) -> Self {
        Self(self.0 / divisor)
    }
}

impl fmt::Display for Depth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}
