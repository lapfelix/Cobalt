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

The fixture uses a dummy named API secret and never makes real catalog or
borrowing calls. `queue-offline.kobo` deliberately switches the simulator to
offline mode and verifies the network error presentation.
