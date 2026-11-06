/*
 * MIT License
 *
 * Copyright (c) 2026 Roberto Leinardi
 *
 * Permission is hereby granted, free of charge, to any person obtaining a copy
 * of this software and associated documentation files (the "Software"), to deal
 * in the Software without restriction, including without limitation the rights
 * to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
 * copies of the Software, and to permit persons to whom the Software is
 * furnished to do so, subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be included in all
 * copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
 * AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
 * OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
 * SOFTWARE.
 */

//! The registry's two parity gains from rust-common's go-prom, each against the old in-tree
//! registry's behaviour: a collector's descriptors are read once, at registration, and outside the
//! registry's lock, as the Prometheus Go client's `Register` reads `Describe` before it locks.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Weak};
use std::thread;
use std::time::Duration;

use go_prom::{Collector, Desc, Family, Kind, Registry};

/// A collector that may inspect its registry while it is described, and counts its descriptions.
struct Probe {
    registry: Weak<Registry>,
    desc: Arc<Desc>,
    described: AtomicUsize,
}

impl Probe {
    /// A probe that calls back into `registry` (an empty `Weak` for none).
    fn new(registry: Weak<Registry>, name: &str) -> Arc<Self> {
        Arc::new(Self {
            registry,
            desc: Desc::new(name, "h", Kind::Gauge, &[]).unwrap_or_else(|error| panic!("{error}")),
            described: AtomicUsize::new(0),
        })
    }
}

impl Collector for Probe {
    fn descs(&self) -> Vec<Arc<Desc>> {
        self.described.fetch_add(1, Ordering::SeqCst);
        if let Some(registry) = self.registry.upgrade() {
            let _ = registry.described_names();
        }
        vec![Arc::clone(&self.desc)]
    }

    fn collect(&self, _families: &mut Vec<Family>) {}
}

/// Describing a collector may call back into its registry: the old registry held its lock while
/// it described a new collector, so this registration deadlocked.
#[test]
fn a_collector_may_inspect_its_registry_while_it_is_registered() {
    let registry = Arc::new(Registry::new());
    let worker_registry = Arc::clone(&registry);
    let (sent, received) = mpsc::channel();
    // Detached: on a deadlock the thread never returns, and the timeout below fails the test.
    thread::spawn(move || {
        for name in ["first", "second"] {
            let probe = Probe::new(Arc::downgrade(&worker_registry), name);
            worker_registry.register(probe).unwrap();
        }
        sent.send(()).unwrap();
    });
    received
        .recv_timeout(Duration::from_secs(5))
        .expect("registering a collector that inspects its registry deadlocked or failed");
    assert_eq!(
        registry.described_names(),
        ["first".to_owned(), "second".to_owned()].into()
    );
}

/// A collector is described once, when it is registered: the old registry described every
/// registered collector again on each later registration and on each `described_names`.
#[test]
fn a_collector_is_described_once_at_registration() {
    let registry = Arc::new(Registry::new());
    // Not linked back to the registry, so the count is the registry's own calls.
    let first = Probe::new(Weak::new(), "first");
    let second = Probe::new(Weak::new(), "second");
    registry
        .register(Arc::clone(&first) as Arc<dyn Collector>)
        .unwrap();
    registry
        .register(Arc::clone(&second) as Arc<dyn Collector>)
        .unwrap();
    let _ = registry.described_names();
    let _ = registry.gather();
    assert_eq!(first.described.load(Ordering::SeqCst), 1, "first");
    assert_eq!(second.described.load(Ordering::SeqCst), 1, "second");
}
