use serde::ser::{Serialize, Serializer};

/// One of the available colors for a graph.
#[derive(Debug, Clone, Copy, PartialOrd, Ord, PartialEq, Eq)]
pub enum GraphColor {
    Blue,
    Green,
    Grey,
    Ink,
    Magenta,
    Orange,
    Purple,
    Red,
    Teal,
    Yellow,
}

impl Serialize for GraphColor {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            GraphColor::Blue => "blue".serialize(serializer),
            GraphColor::Green => "green".serialize(serializer),
            GraphColor::Grey => "grey".serialize(serializer),
            GraphColor::Ink => "ink".serialize(serializer),
            GraphColor::Magenta => "magenta".serialize(serializer),
            GraphColor::Orange => "orange".serialize(serializer),
            GraphColor::Purple => "purple".serialize(serializer),
            GraphColor::Red => "red".serialize(serializer),
            GraphColor::Teal => "teal".serialize(serializer),
            GraphColor::Yellow => "yellow".serialize(serializer),
        }
    }
}
