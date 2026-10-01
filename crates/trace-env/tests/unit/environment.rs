use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::os::{EnvVars, Platform};

/// Counts detections; finds nothing.
struct Counting(AtomicUsize);

impl Ecosystem for Counting {
    fn id(&self) -> EcosystemId {
        EcosystemId::Python
    }
    fn accepts_env_path(&self, _path: &Path) -> bool {
        false
    }
    fn toolchain(&self, _cx: &DetectContext<'_>) -> Found<Toolchain> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Found::Missing {
            searched: vec!["PATH".into()],
        }
    }
    fn deps(&self, _read: &DetectContext<'_>, _toolchain: Option<&Toolchain>) -> Dependencies {
        DepsReport::none_declared()
    }
}

/// A detection is reused while the inputs and the `--env` path are unchanged, and redone
/// when either changes.
#[test]
fn rule_detections_are_cached_per_repository_until_an_input_changes() {
    let tmp = tempfile::tempdir().unwrap();
    let (platform, vars) = (Platform::current(), EnvVars::default());
    let cx = DetectContext {
        root: tmp.path(),
        platform: &platform,
        vars: &vars,
        env_override: None,
        forbidden: &[],
        files: &[],
    };
    let eco = Counting(AtomicUsize::new(0));
    let cache = DetectionCache::default();
    let first = cache.detect(&eco, &cx, &[], "lock-1");
    assert_eq!(first.source, None);
    assert_eq!(first.build, BuildState::NotChecked);
    assert_eq!(cache.detect(&eco, &cx, &[], "lock-1"), first);
    assert_eq!(eco.0.load(Ordering::SeqCst), 1, "reused");
    cache.detect(&eco, &cx, &[], "lock-2");
    assert_eq!(eco.0.load(Ordering::SeqCst), 2, "a changed lockfile re-detects");
    let env_dir = tmp.path().join("venv");
    std::fs::create_dir_all(&env_dir).unwrap();
    let with_env = DetectContext {
        env_override: Some(&env_dir),
        ..cx
    };
    cache.detect(&eco, &with_env, &[], "lock-2");
    assert_eq!(eco.0.load(Ordering::SeqCst), 3, "an --env path re-detects");
    cache.forget(tmp.path());
    cache.detect(&eco, &cx, &[], "lock-2");
    assert_eq!(eco.0.load(Ordering::SeqCst), 4, "forgotten");
}

#[test]
fn rule_every_ecosystem_is_registered_under_its_id() {
    for id in EcosystemId::ALL {
        assert_eq!(id.ecosystem().id(), id);
    }
    assert_eq!(ORDER[0], Where::Remembered);
    assert_eq!(Origin::Override.step(), Where::Remembered);
    assert_eq!(Origin::UserCache.step(), Where::Standard);
}
