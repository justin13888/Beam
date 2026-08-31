use kynos::Schema;
use serde::Serialize;

#[derive(Clone, Debug, Serialize, serde::Deserialize, Schema)]
pub struct Resolution {
    pub width: u32,
    pub height: u32,
}

impl From<&crate::utils::format::Resolution> for Resolution {
    fn from(res: &crate::utils::format::Resolution) -> Self {
        Self {
            width: res.width,
            height: res.height,
        }
    }
}
