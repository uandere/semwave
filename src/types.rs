use std::borrow::Borrow;
use std::fmt;
use std::str::FromStr;

use crate::semver::Bump;

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CrateName(String);

impl CrateName {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CrateName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for CrateName {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for CrateName {
    fn from(s: &str) -> Self {
        Self(s.to_owned())
    }
}

impl AsRef<str> for CrateName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl Borrow<str> for CrateName {
    fn borrow(&self) -> &str {
        &self.0
    }
}

/// Crate name normalized to Rust identifier form (hyphens replaced with
/// underscores), matching the convention used by rustdoc / rustc.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RustdocCrateName(String);

impl fmt::Display for RustdocCrateName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&CrateName> for RustdocCrateName {
    fn from(name: &CrateName) -> Self {
        Self(name.as_str().replace('-', "_"))
    }
}

impl From<String> for RustdocCrateName {
    fn from(s: String) -> Self {
        Self(s.replace('-', "_"))
    }
}

impl From<&str> for RustdocCrateName {
    fn from(s: &str) -> Self {
        Self(s.replace('-', "_"))
    }
}

impl Borrow<str> for RustdocCrateName {
    fn borrow(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ManifestPath(String);

impl ManifestPath {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ManifestPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for ManifestPath {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl AsRef<str> for ManifestPath {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone)]
pub struct Toolchain(String);

impl Toolchain {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Toolchain {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for Toolchain {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl AsRef<str> for Toolchain {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl FromStr for Toolchain {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(s.to_owned()))
    }
}

#[derive(Debug, Clone)]
pub struct UnderBumpedItem<'a> {
    pub name: &'a CrateName,
    pub required: Bump,
    pub local: Bump,
}

#[derive(Debug, Clone)]
pub struct MissingBumpItem<'a> {
    pub name: &'a CrateName,
    pub required: Bump,
}

#[derive(Debug, Clone)]
pub struct TreeEdge {
    pub child: CrateName,
    pub bump: Bump,
}
