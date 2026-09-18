//! Interned tags for unknown enum variants (39 §2.2).
//!
//! The persisted timeline model has several open-ended enums (`EffectKind`,
//! `TransitionKind`, `AudioFxKind`, `GradeOpKind`, …) that serialize as bare
//! snake_case strings. When a document written by a newer build carries a
//! variant this build does not understand, we must preserve the original
//! serialized tag *verbatim* and re-emit it unchanged on save (39 §2.2 rule 1:
//! "never drop, never guess").
//!
//! The four fieldless discriminant enums are `Copy` and are passed by value
//! everywhere (e.g. `PropTargetKind::Effect(effect.kind)` moves out of a
//! `&mut ClipEffect` borrow and requires `Copy`). Adding `Unknown(String)`
//! would drop `Copy` and cascade into ~100 call sites. [`UnknownTag`] sidesteps
//! that: it is a `Copy` handle (a `u32`) into a process-wide intern table, so
//! the enums that carry it stay `Copy`.
//!
//! ## Interning and the deliberate leak
//! New tags are `Box::leak`ed into `'static` storage and deduplicated. The
//! table never shrinks. This is a bounded leak: growth is capped by the number
//! of *distinct* unknown tags seen across all loaded documents — tens, not
//! thousands, since an unknown tag only exists when a file references a variant
//! this build lacks. The alternative (an `Arc<str>` handle) would cost `Copy`
//! and the point of this type is to keep `Copy`.
//!
//! Invariants:
//! - (a) `intern(s).as_str() == s` byte-for-byte, always — this is the
//!   verbatim-preservation guarantee for tags.
//! - (b) `intern(a) == intern(b)` iff `a == b`.
//! - (c) the table never shrinks.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// An interned serialized tag for a variant this build does not understand
/// (39 §2.2). `Copy` so the discriminant enums that carry it stay `Copy`.
#[derive(Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct UnknownTag(u32);

type Intern = (Vec<&'static str>, HashMap<&'static str, u32>);

fn table() -> &'static Mutex<Intern> {
    static INTERN: OnceLock<Mutex<Intern>> = OnceLock::new();
    INTERN.get_or_init(|| Mutex::new((Vec::new(), HashMap::new())))
}

impl UnknownTag {
    /// Intern `s`, returning a `Copy` handle. Interning the same string twice
    /// yields an equal handle (invariant b); the returned handle's [`as_str`]
    /// is byte-for-byte `s` (invariant a).
    ///
    /// [`as_str`]: UnknownTag::as_str
    pub fn intern(s: &str) -> Self {
        let mut guard = table().lock().expect("unknown-tag intern table poisoned");
        if let Some(&id) = guard.1.get(s) {
            return UnknownTag(id);
        }
        // Leak a `'static` copy so both the vec and the map key point at the
        // same allocation; the table never shrinks (invariant c).
        let leaked: &'static str = Box::leak(s.to_owned().into_boxed_str());
        let id = guard.0.len() as u32;
        guard.0.push(leaked);
        guard.1.insert(leaked, id);
        UnknownTag(id)
    }

    /// The original serialized tag, verbatim.
    pub fn as_str(self) -> &'static str {
        let guard = table().lock().expect("unknown-tag intern table poisoned");
        guard.0[self.0 as usize]
    }
}

impl std::fmt::Debug for UnknownTag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "UnknownTag({:?})", self.as_str())
    }
}

impl std::fmt::Display for UnknownTag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for UnknownTag {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for UnknownTag {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Ok(UnknownTag::intern(&s))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_identity() {
        assert_eq!(UnknownTag::intern("film_look").as_str(), "film_look");
        assert_eq!(UnknownTag::intern("").as_str(), "");
        assert_eq!(UnknownTag::intern("iris_wipe 🌀").as_str(), "iris_wipe 🌀");
    }

    #[test]
    fn interning_dedups_and_compares() {
        let a = UnknownTag::intern("caustics");
        let b = UnknownTag::intern("caustics");
        let c = UnknownTag::intern("holo_gen");
        assert_eq!(a, b, "same string interns to an equal handle");
        assert_ne!(a, c, "distinct strings intern to distinct handles");
        assert_eq!(a.0, b.0, "dedup: no new slot for a repeat");
    }

    #[test]
    fn serde_round_trip_is_bare_string() {
        let t = UnknownTag::intern("film_look");
        let json = serde_json::to_string(&t).unwrap();
        assert_eq!(json, "\"film_look\"");
        let back: UnknownTag = serde_json::from_str(&json).unwrap();
        assert_eq!(back, t);
        assert_eq!(back.as_str(), "film_look");
    }

    #[test]
    fn is_copy() {
        // A function taking `UnknownTag` by value; calling it twice on the same
        // binding compiles only if `UnknownTag: Copy`.
        fn takes(t: UnknownTag) -> &'static str {
            t.as_str()
        }
        let t = UnknownTag::intern("exciter");
        assert_eq!(takes(t), "exciter");
        assert_eq!(takes(t), "exciter");
    }
}
