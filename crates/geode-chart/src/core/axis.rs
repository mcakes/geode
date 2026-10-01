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

    /// The one axis a `(pane, side)` pair names.
    pub fn of(pane: Pane, side: Side) -> Axis {
        match (pane, side) {
            (Pane::Upper, Side::Left) => Axis::Left,
            (Pane::Upper, Side::Right) => Axis::Right,
            (Pane::Lower, Side::Left) => Axis::BottomLeft,
            (Pane::Lower, Side::Right) => Axis::BottomRight,
        }
    }

    /// The axis's position in [`Axis::ALL`]: the index of whatever is
    /// kept one per axis, an element's side scales and a model's y formats
    /// alike.
    pub fn index(self) -> usize {
        match self {
            Axis::Left => 0,
            Axis::Right => 1,
            Axis::BottomLeft => 2,
            Axis::BottomRight => 3,
        }
    }

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

impl Pane {
    /// The pane's position among an element's per-pane state: the upper
    /// pane first.
    pub fn index(self) -> usize {
        match self {
            Pane::Upper => 0,
            Pane::Lower => 1,
        }
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

    #[test]
    fn an_axis_is_indexed_in_all_order_and_named_by_its_pane_and_side() {
        for (i, axis) in Axis::ALL.into_iter().enumerate() {
            assert_eq!(axis.index(), i, "{axis:?}");
            assert_eq!(Axis::of(axis.pane(), axis.side()), axis);
        }
        assert_eq!(Pane::Upper.index(), 0);
        assert_eq!(Pane::Lower.index(), 1);
    }
}
