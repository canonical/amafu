use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MachineShape {
    Small,
    Medium,
    Large,
    Xlarge,
}

impl MachineShape {
    #[must_use]
    pub fn cpu_count(self) -> u16 {
        match self {
            Self::Small => 2,
            Self::Medium => 4,
            Self::Large => 8,
            Self::Xlarge => 16,
        }
    }

    #[must_use]
    pub fn memory_mebibytes(self) -> u32 {
        u32::from(self.cpu_count()) * 2048
    }

    #[must_use]
    pub fn disk_gibibytes(self) -> u32 {
        match self {
            Self::Small => 20,
            Self::Medium => 40,
            Self::Large => 80,
            Self::Xlarge => 160,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_shape_doubles_the_one_before() {
        let shapes = [MachineShape::Small, MachineShape::Medium, MachineShape::Large, MachineShape::Xlarge];
        for pair in shapes.windows(2) {
            assert_eq!(pair[1].cpu_count(), pair[0].cpu_count() * 2);
            assert_eq!(pair[1].memory_mebibytes(), pair[0].memory_mebibytes() * 2);
        }
    }
}
