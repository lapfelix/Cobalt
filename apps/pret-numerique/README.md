# Prêt numérique Kobo client

This Cobalt app is a thin client for the server-side Prêt numérique proxy. It
searches Montréal and BAnQ, keeps only opaque publication handles in Cobalt
storage, and never requests an LCPL, EPUB, or publication URL.

There are three screens: Search, Library and Settings. There is no list of
jobs, because a list of jobs is not a thing anyone wants to read.

The bottom bar carries a fourth slot, `Kobo reader`, which ends the Cobalt
session and gives the panel back. It is there because a NickelMenu entry can
present this app directly instead of the launcher, and an app presented that
way is home: the runtime draws no back control of its own, so without that slot
the only way out would be a power cycle. Cobalt's own Settings, Terminal and
Store stay reachable through a second menu entry that presents the launcher.

Choosing a library on a book's detail screen borrows it. There is no
confirmation step: picking which library to borrow from is already the decision,
and the app has a return flow if it was the wrong one. The request goes out on
that tap, the progress appears under the title on the same frame, and the
outcome is reported there. A return is followed the same way on Library, but a
return is confirmed first, because it gives up a loan and the proxy cannot take
it back. Either way the app polls the one job it started, every two seconds, and
stops the moment the job settles, the reader navigates away, or the app goes to
the background.

Successes land in Library, which is where a reader would look for a loan
anyway. The outcomes that need a person go where they belong:

- A delivery failure (`hook_failed`) is a failure to copy the book to the
  reader; the loan and the licence are both intact. It appears on that loan's
  Library row as `Not sent to your reader`, and tapping the row opens a screen
  with a labelled `Send to my reader again`.
- A failed or unconfirmed return appears on the loan's row from
  `return_state`. A failed return can simply be tried again; an unconfirmed one
  is a dead end until it is acknowledged.
- A signed-out catalogue cannot be fixed from the Kobo at all, so Settings
  names the command that fixes it on the home server.
- A borrow the library never confirmed, and a request that stopped outright,
  have no loan to sit against. Those appear in a `Needs your attention` section
  of Library that exists only while something is unresolved, and draws nothing
  otherwise.

Acknowledging is the only way past a borrow or return the proxy refuses to
guess about: `POST /jobs` answers 409 for an unresolved `borrow_uncertain`, and
`POST /books/{id}/return` answers 409 for `return_uncertain`. So it is an
explicit button behind an explanation that tells the reader to check their
library account first, never a cross on a row and never automatic.

Every request that changes something on the server -- borrow, return, resend,
acknowledge -- uses a single non-retrying task. Search, library, job and health
reads use the retrying path. Acknowledging is idempotent on the proxy, but it
still goes through the non-retrying path: a decision a person made once should
not be replayed by the runtime behind their back, and the button is still there
if the network dropped.

The API bearer token is supplied through Cobalt's named
`pret-numerique-api` secret. Library credentials and signed catalog links stay
on `.202`.

## Drive scripts

The scripts in `tests/` are intended for `kobo drive` against a running
Prêt numérique simulator and a deterministic proxy fixture. Start that fixture
with the measured Clara 2E profile from the app directory:

```text
KOBO_SIM_PROFILE=n506 KOBO_SIM_PRET_NUMERIQUE_FIXTURE=1 \
  ../../target/debug/kobo dev 127.0.0.1:8787
```

Then run, for example:

```text
../../target/debug/kobo drive --address 127.0.0.1:8787 \
  --script tests/library-return.kobo
```

Each script expects a simulator it has to itself, started fresh: the app
remembers the last search in its UI-state store, and the fixture remembers what
has been acknowledged and resent for as long as the simulator process lives.
Restart `kobo dev` and clear the simulator's state store between scripts.

`leave-to-reader.kobo` ends the session on its last step, so the simulator
closes the connection and there is nothing after it to drive.

The fixture uses a dummy named API secret and never makes real catalog or
borrowing calls. It holds Montréal signed out so `needs-attention.kobo` can
reach the Settings advice, and it walks an accepted borrow, return and resend
from `queued` to a settled state one poll at a time so the inline progress is
actually driven. `health-offline.kobo` and `borrow-offline.kobo` switch the
simulator to offline mode: the first checks the health read, the second checks
that a borrow which was never sent says so.
