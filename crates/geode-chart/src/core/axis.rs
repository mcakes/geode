//! Four y-axis assignments across an upper and an optional lower pane.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Axis {
    #[default]
    Left,
    Right,
    BottomLeft,
    BottomRight,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Pane {
    Upper,
    Lower,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Side {
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum AxisMode {
    #[default]
    Session,
    Continuous,
}

impl Axis {
    pub const ALL: [Axis; 4] = [Axis::Left, Axis::Right, Axis::BottomLeft, Axis::BottomRight];

    pub fn pane(self) -> Pane {
        match self {
            Axis::Left | Axis::Right => Pane::Upper,
            Axis::BottomLeft | Axis::BottomRight => Pane::Lower,
        }
    }
    pub fn side(self) -> Side {
        match self {
            Axis::Left | Axis::BottomLeft => Side::Left,
            Axis::Right | Axis::BottomRight => Side::Right,
        }
    }
    /// Cycle `left → right → bottomleft → bottomright → left`.
    pub fn next(self) -> Axis {
        let i = Axis::ALL.iter().position(|a| *a == self).unwrap();
        Axis::ALL[(i + 1) % 4]
    }
    pub fn prev(self) -> Axis {
        let i = Axis::ALL.iter().position(|a| *a == self).unwrap();
        Axis::ALL[(i + 3) % 4]
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Axis::Left => "left",
            Axis::Right => "right",
            Axis::BottomLeft => "bottomleft",
            Axis::BottomRight => "bottomright",
        }
    }
    /// Compact axis label: `L`, `R`, `BL` or `BR`.
    pub fn letter(self) -> &'static str {
        match self {
            Axis::Left => "L",
            Axis::Right => "R",
            Axis::BottomLeft => "BL",
            Axis::BottomRight => "BR",
        }
    }
    pub fn parse(s: &str) -> Option<Axis> {
        Axis::ALL.into_iter().find(|a| a.as_str() == s)
    }
}

impl AxisMode {
    pub fn as_str(self) -> &'static str {
        match self {
            AxisMode::Session => "session",
            AxisMode::Continuous => "time",
        }
    }
    pub fn parse(s: &str) -> Option<AxisMode> {
        match s {
            "session" => Some(AxisMode::Session),
            "time" => Some(AxisMode::Continuous),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn axes_cycle_and_round_trip() {
        assert_eq!(Axis::Left.next(), Axis::Right);
        assert_eq!(Axis::BottomRight.next(), Axis::Left);
        assert_eq!(Axis::Left.prev(), Axis::BottomRight);
        for a in Axis::ALL {
            assert_eq!(Axis::parse(a.as_str()), Some(a));
        }
        assert_eq!(Axis::BottomLeft.pane(), Pane::Lower);
        assert_eq!(Axis::BottomLeft.side(), Side::Left);
        assert_eq!(Axis::Right.pane(), Pane::Upper);
        assert_eq!(Axis::BottomRight.letter(), "BR");
        assert_eq!(AxisMode::parse("time"), Some(AxisMode::Continuous));
        assert_eq!(AxisMode::parse("wall"), None);
    }
}
