# Prêt numérique Kobo client

This Cobalt app is a thin client for the server-side Prêt numérique proxy. It
searches Montréal and BAnQ, keeps only opaque publication handles in Cobalt
storage, and never requests an LCPL, EPUB, or publication URL.

There are three places: Discover, Library and Settings. There is no list of
jobs, because a list of jobs is not a thing anyone wants to read.

Discover is the first screen, and Search is on its own bar rather than in the
bottom one. The bar stays at three destinations and the way out, although there
are more than a dozen screens, because the bar drops destinations it cannot give
a finger's width to: a fifth slot on the narrowest panel would take the way out
with it. Finding a book is one place -- Discover, Search, a subject, an author,
a book, its neighbours -- and the two screens that start a search are the two
the bar's first slot leads to.

Back walks a trail rather than a fixed map of parents, because a book now leads
to its author, an author's list leads to another book, and that book leads on
again. Each step carries the book and the list it was looking at, so going back
four times does not land on the last book opened.

The bottom bar carries a fourth slot, `Kobo reader`, which ends the Cobalt
session and gives the panel back. It is there because a NickelMenu entry can
present this app directly instead of the launcher, and an app presented that
way is home: the runtime draws no back control of its own, so without that slot
the only way out would be a power cycle. Cobalt's own Settings, Terminal and
Store stay reachable through a second menu entry that presents the launcher.

## Paging

E Ink does not scroll, so every long list is paged with `Previous page` and
`Next page` buttons. A list with only one page has no pager band; on a
multi-page list, the unavailable direction remains disabled so the other
button does not move under the reader's thumb between pages. Whether there is
another page of a browse is the server's answer, never a count of the rows in
hand -- a merged list can end on a full page, and a client that guessed from
the count would offer a page that is not there. Lists the app holds all of
(search results, subjects, loans, holds, a book's neighbours) know their own
ends.

How many rows a page holds comes from the panel, which the runtime states during
the handshake: a Clara holds five under a heading and the page controls, a Nia
three, an Elipsa more. Nothing is fixed, because the layout engine stops at the
bottom of the content area and drops the rest in silence, so a page measured for
one panel loses its last rows on a smaller one and wastes half of a larger one.
Library works out what is left after whatever else it has to say today, and a
book's screen carries a taste of its blurb only where there is room for it as
well as the ways on from the book.

## A book's screen

A book's screen is a definition list, the libraries that carry it, and the ways
on from it, and it is the screen with the least room to spare. The facts are
ordered by what a borrow is actually decided on -- the author, where a copy is,
a PDF warning, the rating, how much of the book there is, then the edition --
and cut to what the panel holds: three of them on a six inch panel where both
libraries carry the book, five where only one does, all of them on a ten inch
one. Cut from the end, because what the layout engine does with a publisher's
name that does not fit is drop the borrow button under it and say nothing.

`Length` is one row whichever way the catalogue counted: `312 pages` for a
book, `9 h 12 min` for a recording. Almost nothing has both, a fifth of the
books have neither, and a field the libraries did not send draws no row at all
rather than a row with nothing in it. A publication date is drawn as a month and
a year, never a timestamp: the day is the one part of it that could be wrong,
since half the libraries' dates are an evening in UTC.

## The whole description

Descriptions run to nearly four thousand characters, so the book's screen offers
two lines of one and a row that opens the rest of it full screen, paged with the
same two buttons every list here uses. Where the pages break is measured against
the panel rather than counted in characters -- a page of one long paragraph holds
half again what a page of short ones does -- and the measurement subtracts what
the screen puts under the words, including a banner where there is one to draw.
A page measured as though nothing were ever wrong loses its last lines the first
time something is.

## Availability across the libraries

One library being out of copies is not the book being unavailable. List rows
carry the best answer across both, while the book screen names each library
directly on its borrow/reserve action. A hold is offered instead of a borrow only
when no library has a copy at all, because joining a queue for a book the other
library has on the shelf is a worse answer than borrowing it.

## The shelf

Library is the merged profile: the loans the home server holds, with the date
each is due, read from what the libraries themselves say and matched to the file
the server has by title, because the two lists are answers from two different
systems and only the book is common to both. A loan the library has that the
server has no file for is reported as a sentence rather than a row that leads
nowhere. Holds get a screen of their own, one tap away, with the place in each
queue and the dates around it: a single paged list to a screen is the only way
`Previous` and `Next` can mean one thing. There is no cancel there. The
catalogue says a hold can be cancelled but never says how, so the app does not
guess at a request that would give a place in a queue away; it says where it can
be done.

## PDFs

A PDF on this panel is a photographed paper page, and no amount of type setting
fixes it. A book neither library offers any other way is marked `PDF` on its row
and on its screen, and is left out of a list until it is asked for -- one
`Show PDF` chip, off by default, in the same run as the orders a list can be
read in. Which books those are is the server's answer: a book with an EPUB is
not a PDF book because a PDF exists beside it. Audiobooks and online resources
never appear at all, because the app has no audio and the licence pipeline
produces EPUBs; the server leaves them out and this app asks for nothing else.

## Borrowing, holding and returning

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

A hold is placed the same way, on one tap, and for the same reason: choosing the
library is already the decision, and a hold is no more reversible than a borrow
-- both are undone at the library, not here -- so putting a confirmation in
front of only one of them would teach the reader that the other is somehow less
of a choice. A hold the library never confirmed (`hold_uncertain`) joins the
other things a person has to settle in `Needs your attention`.

A list being read is dropped when the reader asks for something else, including
when they tap a library to borrow: a read is idempotent, nothing has been told
to change, and it can be asked for again. Something already changing on the
server has to finish first, and says so. Before this, reading the home page on
startup was enough to make the first tap on a library answer "still finishing
the last request".

Every request that changes something on the server -- borrow, hold, return,
resend, acknowledge -- uses a single non-retrying task. Search, library, job and health
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

What the fixture answers is drawn from a real capture of both catalogues: a
merged home page whose first list both libraries publish, a book out at Montréal
and free at BAnQ, one no library has free at all, an author with several books,
a three-page browse, a shelf with due dates and two queues, one title that
is a PDF and nothing else, a blurb long enough to page through, an audiobook
with a running time and no page count, and one title the catalogue counted
nothing about.

## What the proxy is assumed to answer

The proxy is specified by name and by shape but not field by field, so the
parsers read every spelling that was plausible rather than one:

- a list of books arrives under `results`, `publications`, `books` or `items`,
  or as a bare array;
- an author is a name or an object with a `name`, and a browse of that author is
  addressed by the name itself, which is what the catalogue's own author links
  are keyed on;
- whether a browse has another page comes from `has_next`, `next_page` or
  `next`, and the same for `previous`; `total_pages` is drawn only if given;
- a subject carries `key`, `category` or `id`, falling back to its name;
- the shelf is a bare array or one under `entries`, `items` or `shelf`, and a
  hold's position is either at the top level or under `holds`;
- the edition is `published` (any ISO date, or a bare year), `number_of_pages`
  or `numberOfPages`, `publisher` as a string or an object with a `name`, and
  `duration` in whole seconds; each is dropped rather than drawn when it arrives
  empty or zero;
- PDF-only is `pdf_only`, a `format` of `pdf`, or a `formats` array of nothing
  but PDF. Asking for them anyway is `include_pdf=1` on `/browse` and
  `"include_pdf": true` in the `/search` body -- the one parameter name here
  that the contract does not fix, so it is the first thing to check against the
  proxy.

A hold that succeeds is taken to settle as `complete`, or as `held` or
`hold_placed` if the proxy names it; `holding` is treated as still running. Only
`hold_uncertain` is specified.

`leave-to-reader.kobo` ends the session on its last step, so the simulator
closes the connection and there is nothing after it to drive.

The fixture uses a dummy named API secret and never makes real catalog or
borrowing calls. It holds Montréal signed out so `needs-attention.kobo` can
reach the Settings advice, and it walks an accepted borrow, return and resend
from `queued` to a settled state one poll at a time so the inline progress is
actually driven. `health-offline.kobo` and `borrow-offline.kobo` switch the
simulator to offline mode: the first checks the health read, the second checks
that a borrow which was never sent says so.
