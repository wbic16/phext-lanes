//! Who may create a fabric.
//!
//! Tessera, Will's founding-plot registry, will answer this later. Until
//! that integration exists, [`AdminTokenCheck`] is the default: everyone is
//! refused except a caller who presents `LANES_ADMIN_TOKEN`.

/// Proof that the caller holds a founding plot.
///
/// `proof` is the secret the request presented. An empty proof is never
/// enough. A future Tessera check can replace the admin-token implementation
/// without changing the call sites.
pub trait PlotHolderCheck: Send + Sync {
    fn plot_holder_check(&self, proof: &str) -> bool;
}

/// Deny everyone except a bearer of the configured admin token.
pub struct AdminTokenCheck {
    token: Option<String>,
}

impl AdminTokenCheck {
    pub fn from_env() -> Self {
        let token = std::env::var("LANES_ADMIN_TOKEN")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        Self { token }
    }

    pub fn from_token(token: impl Into<String>) -> Self {
        let token = token.into();
        let token = token.trim();
        Self {
            token: if token.is_empty() {
                None
            } else {
                Some(token.to_string())
            },
        }
    }
}

impl std::fmt::Debug for AdminTokenCheck {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AdminTokenCheck")
    }
}

impl PlotHolderCheck for AdminTokenCheck {
    fn plot_holder_check(&self, proof: &str) -> bool {
        let Some(expected) = self.token.as_deref() else {
            return false;
        };
        token_eq(expected, proof.trim())
    }
}

pub(crate) fn token_eq(expected: &str, given: &str) -> bool {
    let left = expected.as_bytes();
    let right = given.as_bytes();
    if left.is_empty() {
        return false;
    }
    let mut diff = left.len() ^ right.len();
    let span = left.len().max(right.len());
    for index in 0..span {
        let a = if index < left.len() { left[index] } else { 0 };
        let b = if index < right.len() { right[index] } else { 0 };
        diff |= usize::from(a ^ b);
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_admin_token_is_a_plot_holder() {
        let check = AdminTokenCheck::from_token("plot-secret");
        assert!(check.plot_holder_check("plot-secret"));
        assert!(!check.plot_holder_check(""));
        assert!(!check.plot_holder_check("plot-secre"));
        assert!(!check.plot_holder_check("plot-secret-extra"));
        assert!(!check.plot_holder_check("other-secret"));
        assert!(!AdminTokenCheck::from_token("").plot_holder_check(""));
        assert!(!AdminTokenCheck::from_token("   ").plot_holder_check("plot-secret"));
    }
}
