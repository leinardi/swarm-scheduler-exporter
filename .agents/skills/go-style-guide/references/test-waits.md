# Waiting in tests: classify before you write a sleep

Back to [SKILL.md](../SKILL.md). Tests use no `time.Sleep` unless a site really is one of the
sleep classes below. Tests reach for a sleep for five different reasons, and only some of them
justify one: decide which of these a site is *before* writing it — the class dictates the shape.

**Positive eventual — never a sleep.** "Something another goroutine will do has happened": the
reconciler updated a gauge, the listener returned after cancellation, the stream reconnected.
Wait on the signal, or poll with a deadline: a slow machine then costs milliseconds instead of
flaking, and the failure names the contract that was broken rather than "unexpected nil".

- When the goroutine signals completion on a channel, `select` on it against a timeout and
  `t.Fatal` naming what never happened (`TestListenSwarmEvents_CancelDuringPump_NoReconnectCounted`).
- When the only observable is state (a gauge value, a call counter behind `fakeDocker.mu`), use
  `eventually(t, what, cond)` in `internal/collector` or `eventually(t, timeout, check)` in the
  integration suite. A hand-rolled deadline loop in a test body is this class too: use the
  helper.

This class needs something *observable* to poll. Where the only honest observable is unexported,
prefer a small read-only seam on the production type over poking at internals — or record the
call in the fake and poll that.

**Negative assertion — bounded and commented.** "Nothing happens": no second event connection
after a clean stream. There is no condition to poll for; give the wrong behavior a bounded window
to appear, then assert it did not, and say so in a comment so the next reader does not "fix" it
into a wait that cannot exist. Watching the window on a ticker and failing as soon as the wrong
thing appears (`assertNoSecondEventsConnection` in `engine_wire_test.go`) beats a sleep.

**Real elapsed window — a sleep, and the duration is the point.** A backoff step, a staleness
window. Shortening it changes what is asserted. Name it as a constant or a multiple of the
interval under test, never a bare literal chosen by feel.

**Ordering barrier with no quiescence signal — a sleep, and say why no seam exists.** These are
the ones worth revisiting when a seam appears; the comment is what makes that possible.

**Poll tick inside an eventual-wait helper — already correct.** The ticker inside `eventually`.

Two shapes are always wrong: a sleep whose comment says "give X time to Y" where Y is observable,
and a sleep added to make a flaky test pass without deciding which class it belongs to.
