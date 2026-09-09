extern crate self as recap_ng;

use std::fmt::Display;

pub use recap_ng_derive::Recap;
#[doc(hidden)]
pub use regex;
#[cfg(feature = "schemars")]
#[doc(hidden)]
pub use schemars;
#[doc(hidden)]
pub use serde;

#[derive(Debug, snafu::Snafu)]
pub enum Error {
    NoMatch,
    Field { name: &'static str, error: String },
}

impl Error {
    pub fn field(name: &'static str, e: impl Display) -> Self {
        Self::Field {
            name,
            error: e.to_string(),
        }
    }
}

#[test]
fn test() {
    #[derive(Recap)]
    #[recap(regex = r#"^_(?P<inner>.*?)_$"#)]
    struct Test {
        inner: String,
    }
    let test = "_abc_".parse::<Test>().unwrap();
    assert_eq!(test.inner, "abc");
    assert_eq!(test.to_string(), "_abc_");
}
