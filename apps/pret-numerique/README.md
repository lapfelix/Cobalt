# Prêt numérique Kobo client

This Cobalt app is a thin client for the server-side Prêt numérique proxy. It
searches Montréal and BAnQ, keeps only opaque publication handles and job IDs
in Cobalt storage, and never requests an LCPL, EPUB, or publication URL.

The Library screen has `All`, `Montréal`, and `BAnQ` filters. Selecting a saved
loan always opens a confirmation screen before `POST /books/{id}/return` is
sent. Borrow and return are state-changing requests, so the app uses a single
non-retrying task for each; safe search, library, queue, and health reads use
the retrying task path. A hook failure is surfaced in Queue with a retry-hook
action, while an authentication-required job remains visible until the server
session is renewed.

The API bearer token is supplied through Cobalt's named
`pret-numerique-api` secret. Library credentials and signed catalog links stay
on `.202`.

## Drive scripts

The scripts in `tests/` are intended for `kobo drive` against a running
Prêt numérique simulator and a deterministic proxy fixture. The fixture-backed
scripts document the expected title/job names in their comments; they do not
make real catalog or borrowing calls. `queue-offline.kobo` can run without
catalog fixtures and verifies the offline health presentation.
