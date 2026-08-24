//! A Kobo-only client for the server-side Prêt numérique proxy.
//!
//! The reader holds only opaque publication handles and the last search it was
//! given. It never receives an OPDS URL, an LCPL, an EPUB, or either library's
//! login material, and it keeps no record of a request between launches: the
//! server owns that list and is asked for it. Every request that changes
//! something on the server uses `spawn`, while searches and status reads use
//! `spawn_retrying`.
//!
//! There is no screen listing jobs. A borrow, a hold or a return is followed
//! where the reader started it, a success lands in Library because that is
//! where they would look, and the outcomes only a person can settle appear
//! against the loan they belong to -- or, when there is no loan to attach them
//! to, in a section of Library that exists only while something is unresolved.
//!
//! E Ink does not scroll, so every long list is paged with buttons, and how
//! many rows a page holds comes from the panel the runtime says it is drawing
//! to. Whether a browse has another page is the server's answer rather than a
//! count of the rows in hand. A book's description is paged the same way, but
//! where its pages break is measured against the panel: prose is not rows, and
//! a page of one long paragraph holds half again what a page of short ones
//! does.

use kobo_json::{ObjectBuilder, Value};
use kobo_sdk::keyboard::{TextEntry, Typing};
use kobo_sdk::{
    action_id, BannerLevel, Context, Credential, Failure, Glyph, Header, KoboApp, PictureHandle,
    Screen, ScreenBuilder, Space, StoreResult, Task, TaskError, TaskId, TaskOutcome, TilePicture,
};
use std::process::ExitCode;

const API: &str = "https://home.lapal.me:3300/pret/v1";
const API_SECRET: &str = "pret-numerique-api";
const STORE_STATE: &str = "ui-state";
const COVER_PICTURE_HANDLE: PictureHandle = PictureHandle(41);
const COVER_WIDTH: u32 = 260;
const COVER_HEIGHT: u32 = 419;
const COVER_MAX_BYTES: u32 = 1_300_000;
const MAX_RESULTS: usize = 40;
const MAX_QUERY_CHARS: usize = 80;
const MAX_DETAIL_DESCRIPTION_CHARS: usize = 88;
/// The whole of a blurb, kept because there is now a screen that shows it.
///
/// The libraries' longest runs to a little under 3,900 characters, so this
/// holds every one of them with room to spare. It was 720, which is the median
/// and was the right bound while a book's screen only ever showed a teaser: it
/// cut two thirds off the longest descriptions before anything could page
/// through them.
const MAX_DESCRIPTION_CHARS: usize = 4000;
const MAX_ERROR_CHARS: usize = 160;
const MAX_JOBS: usize = 24;
const POLL_SECONDS: u32 = 2;
/// The floor and ceiling on how many rows a page of a list holds.
///
/// E Ink does not scroll, so every list is paged, and the layout engine stops
/// at the bottom of the content area and drops the rest in silence: a page one
/// row too tall loses that row with nothing on the panel to say so. How many
/// rows a page holds is measured -- see [`PretNumerique::list_rows`] -- and
/// these only bound the answer.
const MIN_PAGE_ROWS: usize = 3;
/// What a book's screen has spent before its first fact, and what the ways on
/// below the libraries cost.
///
/// The furniture is the book's title and the section and line that offer the
/// libraries: everything a borrow needs and none of it optional. A library's own
/// row is charged separately because how many there are is the book's business.
/// A way on is a section and a row together. The blurb's row carries two lines
/// of the blurb where the author's carries a name, and that second line is the
/// difference between a book's screen that fits and one whose last row is drawn
/// under the bar.
///
/// These four are the only measurements still written down rather than asked
/// for, because a book's screen is one arrangement rather than a list of like
/// things and there is nothing on it to measure a page of. They are read off a
/// laid-out screen on the panel this app ships to, and the layout tests hold
/// every panel to what they work out.
const FACT_COST: i32 = 58;
const DETAIL_FURNITURE: i32 = 222;
const LIBRARY_ROW_COST: i32 = 153;
const DETAIL_EXTRA_COST: i32 = 224;
const BLURB_EXTRA_LINE: i32 = 46;
/// The facts a book's screen keeps whatever the panel: who wrote it, and which
/// library has a copy. Nothing about a borrow makes sense without both.
const MIN_DETAIL_FACTS: usize = 2;
/// What a banner costs the page of prose under it.
///
/// Four lines of it, which is what the longest thing this app has to say about
/// a request that failed comes to on the narrowest panel. A page measured as
/// though nothing were ever wrong loses its last lines the first time
/// something is, and the reader has no way to tell that it did.
const NOTE_LINES: i32 = 4;

const MAX_PAGE_ROWS: usize = 8;
/// How many of the libraries' lists Discover draws at once.
///
/// One to a page: a page is then a shelf with some books on it and the way into
/// the rest, rather than two lists with one book each, which says nothing about
/// either.
const DISCOVER_GROUPS_PER_PAGE: usize = 1;
/// The content height above which a book's screen carries its blurb as well as
/// the ways on from it. A Clara has 1190 pixels of content and an Elipsa 1677;
/// a Nia has 842, which is one block short.
const TALL_PANEL: i32 = 1000;
const MAX_GROUPS: usize = 40;
const MAX_CATEGORIES: usize = 40;
const MAX_SHELF: usize = 60;
const DISCOVER: &str = "discover-tab";
const LIBRARY: &str = "library-tab";
const SETTINGS: &str = "settings-tab";
const SEARCH: &str = "open-search";
const CATEGORIES: &str = "open-categories";
const BOOKSTORE_CATEGORIES: &str = "open-bookstore-categories";
const HOLDS: &str = "open-holds";
const SHOW_PDF: &str = "show-pdf";
const FILTERS: &str = "open-filters";
const RELATED: &str = "open-related";
const DESCRIPTION: &str = "open-description";
const BOOKSTORE_CATEGORY: &str = "bookstore-category";
const FAVORITE_CATEGORY: &str = "favorite-category";
const FAVORITE_BROWSE: &str = "favorite-browse";
const PRESET: &str = "browse-preset";
const PREVIOUS_PAGE: &str = "previous-page";
const NEXT_PAGE: &str = "next-page";
const EDIT_QUERY: &str = "edit-query";
const SUBMIT_SEARCH: &str = "submit-search";
const FILTER_ALL: &str = "filter-all";
const FILTER_MONTREAL: &str = "filter-montreal";
const FILTER_BANQ: &str = "filter-banq";
const REFRESH: &str = "refresh";
const CONFIRM_RETURN: &str = "confirm-return";
const RETRY_HOOK: &str = "retry-hook";
const ACKNOWLEDGE: &str = "acknowledge";
const OPEN_LIBRARY: &str = "open-library";
const OPEN_SETTINGS: &str = "open-settings";
const CANCEL: &str = "cancel-action";
const BACK: &str = "back";
const READER: &str = "reader";

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum View {
    /// The first screen: what the libraries are putting forward today.
    #[default]
    Discover,
    Categories,
    /// The cross-reference of the two libraries against leslibraires.ca.
    BookstoreCategories,
    /// One list of books: a discovery group, a category, or an author.
    Browse,
    /// How a list is ordered and what it leaves out, with a screen of its own.
    ///
    /// These were chips on the list itself, and they were unreachable twice
    /// over. The run only appeared while the catalogue had offered more than
    /// one order, so a reply carrying fewer facets than the one before it took
    /// the PDF toggle -- which is this app's own control and nothing to do with
    /// the catalogue -- away with it, and nothing on the screen could bring it
    /// back. And a wrapped run of them cost a line the page budget did not
    /// charge for, which pushed the buttons that turn the page under the bar.
    /// A screen always exists, always draws its own controls, and can say that
    /// something is unavailable instead of not being there.
    Filters,
    Search,
    Results,
    Detail,
    /// One book's blurb with the panel to itself, paged like any other list.
    Description,
    Related,
    Library,
    Holds,
    ConfirmReturn,
    Resolve,
    Settings,
    /// On the panel for the half-minute the stock reader takes to come back.
    Leaving,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum CatalogFilter {
    #[default]
    All,
    Montreal,
    Banq,
}

impl CatalogFilter {
    fn catalog_values(self) -> Vec<&'static str> {
        match self {
            Self::All => vec!["montreal", "banq"],
            Self::Montreal => vec!["montreal"],
            Self::Banq => vec!["banq"],
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::All => "Both libraries",
            Self::Montreal => "Montréal",
            Self::Banq => "BAnQ",
        }
    }

    fn matches(self, catalog: &str) -> bool {
        match self {
            Self::All => true,
            Self::Montreal => catalog == "montreal",
            Self::Banq => catalog == "banq",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Source {
    handle: String,
    catalog: String,
    catalog_name: String,
    availability: String,
    available: bool,
}

/// One book, however the reader arrived at it: a search, a discovery group, a
/// category, an author, or another book's neighbours.
///
/// `handle` is the whole book rather than one library's copy of it, and is what
/// asks the server for its neighbours. Not every list carries one, so a borrow
/// and a hold still go through the per-library handle in `sources`.
#[derive(Clone, Debug, PartialEq)]
struct Publication {
    handle: Option<String>,
    title: String,
    /// Whether the only file either library offers is a PDF.
    ///
    /// The server's answer, never worked out here: a book that offers an EPUB
    /// is not a PDF book because a PDF exists alongside it, and only the server
    /// sees the whole list of what each library has.
    pdf_only: bool,
    authors: Vec<String>,
    isbn: Option<String>,
    description: Option<String>,
    /// When the edition came out, as the server normalised it.
    ///
    /// Each of these four is absent often enough that the screen has to read as
    /// though it were never coming: a fifth of the books have no page count, an
    /// audiobook has one about never, and a book has a listening time about
    /// never. So each is drawn only where there is something to draw, and a
    /// missing one costs no row rather than a row saying nothing.
    published: Option<String>,
    number_of_pages: Option<i64>,
    publisher: Option<String>,
    /// How long an audiobook runs, in seconds.
    duration: Option<i64>,
    goodreads_rating: Option<f64>,
    goodreads_ratings_count: Option<i64>,
    goodreads_reviews_count: Option<i64>,
    sources: Vec<Source>,
}

impl Publication {
    fn available_libraries(&self) -> Vec<&str> {
        self.sources
            .iter()
            .filter(|source| source.available)
            .map(|source| source.catalog_name.as_str())
            .collect()
    }

    fn is_available(&self) -> bool {
        self.sources.iter().any(|source| source.available)
    }

    /// The handle to ask for neighbours with. The book's own if the server gave
    /// one, otherwise any library's copy: what comes back is the same book
    /// either way.
    fn related_handle(&self) -> Option<&str> {
        self.handle
            .as_deref()
            .or_else(|| self.sources.first().map(|source| source.handle.as_str()))
    }
}

/// One list the libraries put forward, after the server merged the same list
/// from every catalogue it can see.
#[derive(Clone, Debug, PartialEq)]
struct Group {
    title: String,
    /// What a browse of the whole list is addressed with, when the server gave
    /// one. Without it the group shows only the books it came with.
    category: Option<String>,
    total: Option<i64>,
    publications: Vec<Publication>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Category {
    name: String,
    key: String,
    libraries: Vec<String>,
    total: Option<i64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct BookstoreCategory {
    id: String,
    name: String,
    path: String,
    held: i64,
    bookstore_total: Option<i64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Sort {
    key: String,
    label: String,
}

/// One run of controls on the filters screen.
///
/// `empty` is what the group says when it has no controls to offer, which is
/// the whole reason this screen exists: a facet the catalogue did not send is a
/// sentence saying there was nothing to choose from, never an absence.
#[derive(Clone, Debug, Eq, PartialEq)]
struct FilterGroup {
    title: &'static str,
    empty: &'static str,
    chips: Vec<(String, String, bool)>,
}

/// What is being browsed, and how. Held whole so that going back to a list
/// lands on the page and the order the reader left it on.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct BrowseQuery {
    title: String,
    category: Option<String>,
    audience: Option<String>,
    author: Option<String>,
    sort: Option<String>,
    page: u32,
    available_only: bool,
    bookstore: bool,
}

/// Where a paged list is, and which way it can still go.
///
/// `total_pages` is drawn only when the server said so. A page count worked out
/// from the rows in hand would be a guess, and a guess that says "page 3 of 3"
/// on a list with forty more pages is worse than saying nothing.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct Paging {
    page: u32,
    total_pages: Option<u32>,
    has_previous: bool,
    has_next: bool,
}

impl Paging {
    /// The bounds of a list the app holds all of, which is not a guess: there
    /// is nothing beyond what is in hand.
    fn sized(page: usize, len: usize, per_page: usize) -> Self {
        let per_page = per_page.max(1);
        let pages = len.div_ceil(per_page).max(1);
        Self {
            page: u32::try_from(page + 1).unwrap_or(1),
            total_pages: u32::try_from(pages).ok(),
            has_previous: page > 0,
            has_next: page + 1 < pages,
        }
    }
}

/// One book the libraries say is yours: out on loan, or waiting for you.
#[derive(Clone, Debug, Eq, PartialEq)]
struct ShelfEntry {
    title: String,
    authors: Vec<String>,
    catalog: String,
    /// `loan` or `hold`. Anything else is not drawn.
    kind: String,
    since: Option<String>,
    until: Option<String>,
    position: Option<i64>,
    total: Option<i64>,
}

impl ShelfEntry {
    fn is_loan(&self) -> bool {
        self.kind == "loan"
    }

    fn is_hold(&self) -> bool {
        self.kind == "hold"
    }
}

/// A screen the reader can be sent back to, with the little it needs to be
/// drawn again.
///
/// A trail rather than a fixed map of parents, because a book now leads to its
/// author, an author's list leads to another book, and that book leads on
/// again. Each step carries the book and the list it was looking at, so going
/// back four times does not land on the last book opened.
#[derive(Clone, Debug, PartialEq)]
struct Place {
    view: View,
    detail: Option<Publication>,
    browse: BrowseQuery,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Book {
    id: String,
    title: String,
    catalog: String,
    file_name: String,
    return_state: Option<String>,
}

/// One request the server is working on or has settled.
///
/// `acknowledged_at` and `dedup_key` are read by nobody here on purpose: the
/// server already omits acknowledged rows from `/jobs` and owns the duplicate
/// guard, so parsing them would only put state on the reader that nothing
/// consults.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Job {
    id: String,
    kind: String,
    state: String,
    title: String,
    catalog: String,
    book_id: Option<String>,
    error: Option<String>,
}

/// The one borrow, return or resend the reader is waiting on.
///
/// `view` is where it was started, and the progress is drawn there and nowhere
/// else. That is also what bounds the poll: leaving the screen drops the watch,
/// and a watch with nothing active never asks for another tick.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Watch {
    view: View,
    job: Option<Job>,
}

impl Watch {
    /// Whether the server is still working on it. A request whose answer has
    /// not arrived counts: it was sent.
    fn is_running(&self) -> bool {
        self.job
            .as_ref()
            .is_none_or(|job| is_active_state(&job.state))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RequestKind {
    Search,
    Discovery,
    CategoryList,
    BookstoreCategories,
    Browse,
    Related,
    Shelf,
    Books,
    Jobs,
    Job,
    Health,
    Cover,
    BookstoreCategory,
    Borrow,
    Hold,
    Return,
    RetryHook,
    Acknowledge,
}

struct PretNumerique {
    view: View,
    trail: Vec<Place>,
    filter: CatalogFilter,
    /// Whether a list may include books only either library only has as a PDF.
    /// Off, because most of them are unreadable on this panel.
    show_pdf: bool,
    query: String,
    entry: TextEntry,
    results: Vec<Publication>,
    results_page: usize,
    groups: Vec<Group>,
    groups_page: usize,
    categories: Vec<Category>,
    categories_page: usize,
    bookstore_categories: Vec<BookstoreCategory>,
    bookstore_categories_page: usize,
    /// IDs only: names and paths come from the daily proxy snapshot.
    favorites: Vec<String>,
    browse: BrowseQuery,
    browsed: Vec<Publication>,
    sorts: Vec<Sort>,
    /// Which page of the filters screen is showing.
    ///
    /// Two groups fit any panel today. It pages anyway, because the catalogue
    /// offers audience, availability, language and format as well, and a screen
    /// that grew a fifth group without paging would lose it the way the chips
    /// it replaced lost the page controls.
    filters_page: usize,
    /// What the server said about the page it just sent.
    browse_paging: Paging,
    /// Which panel-sized slice of that page is drawn. The server decides how
    /// many books a page holds and the panel decides how many fit, so one
    /// server page is several pages here.
    browse_offset: usize,
    /// How many pages the reader has turned in this list, which is the only
    /// page number that cannot be wrong: the server's page size is its own
    /// business.
    browse_number: u32,
    /// Set while fetching the previous server page, so that turning back lands
    /// on its last slice rather than its first.
    browse_from_end: bool,
    related: Vec<Publication>,
    related_page: usize,
    detail: Option<Publication>,
    cover: Option<TilePicture>,
    bookstore_category: Option<String>,
    bookstore_category_state: Option<String>,
    bookstore_category_handle: Option<String>,
    /// Which page of the open book's blurb is showing.
    ///
    /// Not kept on the trail. A `Place` carries the book, so coming back to a
    /// blurb is coming back to its first page, which is where a reader who
    /// left it and returned to it means to start.
    description_page: usize,
    selected_source: usize,
    books: Vec<Book>,
    books_page: usize,
    shelf: Vec<ShelfEntry>,
    holds_page: usize,
    selected_book: Option<usize>,
    jobs: Vec<Job>,
    resolve_job: Option<String>,
    watch: Option<Watch>,
    inflight: Option<(TaskId, RequestKind)>,
    queued_request: Option<RequestKind>,
    sleep_task: Option<TaskId>,
    /// The panel this app is drawing to, learned from the runtime.
    ///
    /// Kept whole rather than as a content height, because a blurb is broken
    /// into pages by measuring it and where the folds land depends on how wide
    /// the panel sets a line as well as how much of it there is. Everything
    /// else asks it only for [`Self::content_height`].
    metrics: kobo_sdk::DisplayMetrics,
    note: Option<String>,
    health: Option<String>,
    health_advice: Option<String>,
    loaded_state: bool,
}

impl Default for PretNumerique {
    fn default() -> Self {
        Self {
            view: View::Discover,
            trail: Vec::new(),
            filter: CatalogFilter::All,
            show_pdf: false,
            query: String::new(),
            entry: TextEntry::new().opened_by(EDIT_QUERY),
            results: Vec::new(),
            results_page: 0,
            groups: Vec::new(),
            groups_page: 0,
            categories: Vec::new(),
            categories_page: 0,
            bookstore_categories: Vec::new(),
            bookstore_categories_page: 0,
            favorites: Vec::new(),
            browse: BrowseQuery::default(),
            browsed: Vec::new(),
            sorts: Vec::new(),
            filters_page: 0,
            browse_paging: Paging::default(),
            browse_offset: 0,
            browse_number: 1,
            browse_from_end: false,
            related: Vec::new(),
            related_page: 0,
            detail: None,
            cover: None,
            bookstore_category: None,
            bookstore_category_state: None,
            bookstore_category_handle: None,
            description_page: 0,
            selected_source: 0,
            books: Vec::new(),
            books_page: 0,
            shelf: Vec::new(),
            holds_page: 0,
            selected_book: None,
            jobs: Vec::new(),
            resolve_job: None,
            watch: None,
            inflight: None,
            queued_request: None,
            sleep_task: None,
            metrics: kobo_sdk::CLARA_BW_METRICS,
            note: None,
            health: None,
            health_advice: None,
            loaded_state: false,
        }
    }
}

impl PretNumerique {
    fn credential() -> Credential {
        Credential::bearer(API_SECRET)
    }

    fn state_bytes(&self) -> Vec<u8> {
        let mut bytes = self.query.replace(['\n', '\r'], " ").into_bytes();
        bytes.push(b'\n');
        bytes.extend_from_slice(self.favorites.join(",").as_bytes());
        bytes.push(b'\n');
        bytes
    }

    fn load_state(&mut self, value: Option<&[u8]>) {
        self.loaded_state = true;
        let Some(value) = value else {
            return;
        };
        let Ok(text) = std::str::from_utf8(value) else {
            return;
        };
        text.lines()
            .next()
            .unwrap_or_default()
            .clone_into(&mut self.query);
        self.favorites = text
            .lines()
            .nth(1)
            .unwrap_or_default()
            .split(',')
            .filter(|id| {
                !id.is_empty()
                    && id.len() <= 8
                    && id
                        .chars()
                        .all(|character| character.is_ascii_alphanumeric())
            })
            .map(str::to_owned)
            .collect();
    }

    fn save_state(&mut self, context: &mut Context) {
        context.store().save(STORE_STATE, self.state_bytes());
    }

    fn show(&self, context: &mut Context) {
        context.set_screen(self.screen());
    }

    fn screen(&self) -> Screen {
        match self.view {
            View::Discover => self.discover_screen(),
            View::Categories => self.categories_screen(),
            View::BookstoreCategories => self.bookstore_categories_screen(),
            View::Browse => self.browse_screen(),
            View::Filters => self.filters_screen(),
            View::Search => self.search_screen(),
            View::Results => self.results_screen(),
            View::Detail => self.detail_screen(),
            View::Description => self.description_screen(),
            View::Related => self.related_screen(),
            View::Library => self.library_screen(),
            View::Holds => self.holds_screen(),
            View::ConfirmReturn => self.confirm_return_screen(),
            View::Resolve => self.resolve_screen(),
            View::Settings => self.settings_screen(),
            View::Leaving => Self::leaving_screen(),
        }
    }

    /// The three places in this app, and the way out of it.
    ///
    /// The reader is a fourth slot rather than a control on one screen because
    /// a menu entry presents this app directly: it is then home, so the runtime
    /// draws no back control of its own and nothing else on any screen leads
    /// out. It is last, furthest from the two slots a finger is on all the
    /// time, and it is never the marked destination -- taking it leaves.
    /// The launcher's own bar calls it the same thing for the same reason: a
    /// bar slot is a quarter of the panel and "Return to Kobo reader" set
    /// across it is a sentence where two words will do.
    ///
    /// It stays four slots although there are more than a dozen screens. The
    /// bar drops destinations it cannot give a finger's width to, so a fifth
    /// slot on the narrowest panel would silently take the way out with it.
    /// Finding a book is one place -- Discover, Search, a category, an author,
    /// a book, its blurb -- and Search is reached from Discover's own bar,
    /// where the reader already is.
    fn nav(screen: ScreenBuilder, selected: usize) -> ScreenBuilder {
        screen.nav_bar(
            Some(selected),
            [
                (DISCOVER, "Discover"),
                (LIBRARY, "Library"),
                (SETTINGS, "Settings"),
                (READER, "Kobo reader"),
            ],
        )
    }

    /// Previous and Next for a list that does not scroll.
    ///
    /// Both controls are always drawn, and the one with nowhere to go is drawn
    /// disabled rather than removed: a control that comes and goes moves the
    /// other one under the reader's thumb between pages.
    fn page_controls(screen: ScreenBuilder, paging: Paging) -> ScreenBuilder {
        let position = match paging.total_pages {
            Some(total) if total > 1 => format!("Page {} of {total}", paging.page),
            _ => format!("Page {}", paging.page),
        };
        let screen = screen.secondary(position);
        screen.band(
            kobo_sdk::BandAlign::Middle,
            [
                (PREVIOUS_PAGE, "Previous page", paging.has_previous),
                (NEXT_PAGE, "Next page", paging.has_next),
            ]
            .map(|(name, label, enabled)| {
                (kobo_sdk::SlotWidth::Fill, move |slot: ScreenBuilder| {
                    slot.button_with_state(
                        name,
                        label,
                        if enabled {
                            kobo_sdk::ControlState::Enabled
                        } else {
                            kobo_sdk::ControlState::Disabled
                        },
                    )
                })
            }),
        )
    }

    fn leaving_screen() -> Screen {
        ScreenBuilder::new("leaving")
            .top_bar("Returning")
            .heading("Returning to the Kobo reader")
            .text("The reader takes about half a minute to start and rescan.")
            .build()
    }

    /// Draws whatever this screen has to say to the reader right now.
    ///
    /// Above the content rather than below it. A note is what an offline borrow
    /// or an unreadable answer has to say, and the layout engine stops at the
    /// bottom of the content area and drops the rest in silence: at the foot of
    /// a full screen, the one line that explains what went wrong is the first
    /// thing to go.
    fn note_block(&self, screen: ScreenBuilder, level: BannerLevel) -> ScreenBuilder {
        match &self.note {
            Some(note) => screen.banner(level, note.clone()),
            None => screen,
        }
    }

    /// Draws the request the reader is waiting on, if this is where they
    /// started it.
    fn watch_block(&self, screen: ScreenBuilder) -> ScreenBuilder {
        let Some(watch) = &self.watch else {
            return screen;
        };
        if watch.view != self.view {
            return screen;
        }
        let Some(job) = &watch.job else {
            return screen.activity("Sending the request...", None);
        };
        if is_active_state(&job.state) {
            return screen.activity(state_label(&job.state), None);
        }
        let (level, message) = settled_message(job);
        let screen = screen.banner(level, message);
        match job.state.as_str() {
            "auth_required" => screen.button(OPEN_SETTINGS, "Open Settings"),
            // Library already carries the loan, the failure and the action, so
            // pointing at it from here would be pointing at this screen.
            _ if self.view == View::Library => screen,
            _ => screen.button(OPEN_LIBRARY, "Open Library"),
        }
    }

    fn search_screen(&self) -> Screen {
        if self.entry.is_open() {
            return ScreenBuilder::new("search-input")
                .top_bar("Search")
                .text_entry(&self.entry, "Search the libraries", "Search")
                .build();
        }
        let mut screen = Self::nav(
            ScreenBuilder::new("search")
                .top_bar("Search")
                .top_bar_action(BACK, "Back"),
            0,
        );
        screen = self.note_block(screen, BannerLevel::Info);
        screen = screen
            .heading("Find a book")
            .text("Borrow from Montréal or BAnQ. Books stay on your home server.")
            .field(EDIT_QUERY, self.query.clone(), "Title, author, or ISBN")
            .field_clear(EDIT_QUERY)
            .chips([
                (FILTER_ALL, "Both", self.filter == CatalogFilter::All),
                (
                    FILTER_MONTREAL,
                    "Montréal",
                    self.filter == CatalogFilter::Montreal,
                ),
                (FILTER_BANQ, "BAnQ", self.filter == CatalogFilter::Banq),
                (SHOW_PDF, "Show PDF", self.show_pdf),
            ])
            .button(SUBMIT_SEARCH, "Search")
            .spacer(Space::Small);
        screen.build()
    }

    fn results_screen(&self) -> Screen {
        let mut screen = Self::nav(
            ScreenBuilder::new("results")
                .top_bar("Results")
                .top_bar_action(REFRESH, "Refresh"),
            0,
        );
        screen = self
            .note_block(screen, BannerLevel::Attention)
            .heading(self.filter.label())
            .text(format!("Search: {}", compact_message(&self.query, 56)));
        if self.awaiting(RequestKind::Search) {
            return screen.skeleton(self.skeleton_lines()).build();
        }
        if self.results.is_empty() {
            screen = screen.empty_state("No titles found.");
        } else {
            let per_page = self.results_rows_per_page();
            let paging = Paging::sized(self.results_page, self.results.len(), per_page);
            screen = self.publication_rows(
                screen,
                "result",
                self.results_page * per_page,
                slice_of(&self.results, self.results_page, per_page),
            );
            screen = Self::page_controls(screen, paging);
        }
        screen.build()
    }

    /// What the libraries are putting forward today, merged into one list of
    /// lists.
    fn discover_screen(&self) -> Screen {
        let mut screen = Self::nav(
            ScreenBuilder::new("discover")
                .top_bar("Prêt numérique")
                .top_bar_action(SEARCH, "Search")
                .top_bar_action(CATEGORIES, "Categories"),
            0,
        );
        screen = self.note_block(screen, BannerLevel::Attention);
        let favorites = self.favorite_categories();
        if !favorites.is_empty() {
            screen = screen.section_rows(
                "Favorite categories",
                None,
                favorites
                    .iter()
                    .take(3)
                    .enumerate()
                    .map(|(index, category)| {
                        (
                            format!("{FAVORITE_BROWSE}.{index}"),
                            self.row_title(&category.path),
                            bookstore_category_summary(category),
                            Glyph::Bookmark,
                        )
                    }),
            );
        }
        if self.groups.is_empty() && self.awaiting(RequestKind::Discovery) {
            return screen.skeleton(self.skeleton_lines()).build();
        }
        if self.groups.is_empty() {
            screen = screen.empty_state("Nothing from the libraries yet.");
        } else {
            let first = self.groups_page * DISCOVER_GROUPS_PER_PAGE;
            for (offset, group) in self
                .groups
                .iter()
                .skip(first)
                .take(DISCOVER_GROUPS_PER_PAGE)
                .enumerate()
            {
                let index = first + offset;
                let books = group
                    .publications
                    .iter()
                    .take(self.discover_books())
                    .enumerate()
                    .map(|(book, publication)| {
                        (
                            format!("group.{index}.book.{book}"),
                            self.row_title(&publication.title),
                            publication_summary(publication),
                            Glyph::Book,
                        )
                    })
                    .chain(group.category.as_ref().map(|_| {
                        (
                            format!("group.{index}"),
                            "See the whole list".to_owned(),
                            group_count(group),
                            Glyph::Circle,
                        )
                    }));
                screen = screen.section_rows(group.title.clone(), None, books);
            }
            screen = Self::page_controls(
                screen,
                Paging {
                    page: u32::try_from(self.groups_page + 1).unwrap_or(1),
                    total_pages: u32::try_from(
                        self.groups.len().div_ceil(DISCOVER_GROUPS_PER_PAGE).max(1),
                    )
                    .ok(),
                    has_previous: self.groups_page > 0,
                    has_next: (self.groups_page + 1) * DISCOVER_GROUPS_PER_PAGE < self.groups.len(),
                },
            );
        }
        screen.build()
    }

    fn categories_screen(&self) -> Screen {
        let mut screen = Self::nav(
            ScreenBuilder::new("categories")
                .top_bar("Categories")
                .top_bar_action(BOOKSTORE_CATEGORIES, "Les Libraires")
                .top_bar_action(BACK, "Back"),
            0,
        )
        .heading("Browse by subject");
        screen = self.note_block(screen, BannerLevel::Attention);
        if self.categories.is_empty() && self.awaiting(RequestKind::CategoryList) {
            return screen.skeleton(self.skeleton_lines()).build();
        }
        if self.categories.is_empty() {
            screen = screen.empty_state("No subjects to browse yet.");
        } else {
            let per_page = self.page_rows();
            let paging = Paging::sized(self.categories_page, self.categories.len(), per_page);
            screen = screen.rows(
                slice_of(&self.categories, self.categories_page, per_page)
                    .iter()
                    .enumerate()
                    .map(|(offset, category)| {
                        (
                            format!("category.{}", self.categories_page * per_page + offset),
                            self.row_title(&category.name),
                            category_summary(category),
                            Glyph::Book,
                        )
                    }),
            );
            screen = Self::page_controls(screen, paging);
        }
        screen.build()
    }

    /// Categories from leslibraires.ca whose books are present in at least one
    /// of the two libraries. Each category gets a browse row and a separate
    /// favorite row so a favorite never steals the action that opens the list.
    fn bookstore_categories_screen(&self) -> Screen {
        let mut screen = Self::nav(
            ScreenBuilder::new("bookstore-categories")
                .top_bar("Les Libraires")
                .top_bar_action(BACK, "Back"),
            0,
        )
        .heading("Browse by bookstore category")
        .text("Only books cross-referenced in Montréal or BAnQ are shown.");
        screen = self.note_block(screen, BannerLevel::Attention);
        if self.bookstore_categories.is_empty() && self.awaiting(RequestKind::BookstoreCategories) {
            return screen.skeleton(self.skeleton_lines()).build();
        }
        if self.bookstore_categories.is_empty() {
            screen = screen.empty_state("No cross-referenced categories yet.");
        } else {
            let per_page = self.bookstore_categories_rows_per_page();
            let page = self
                .bookstore_categories_page
                .min(self.bookstore_categories.len().saturating_sub(1) / per_page);
            let rows = slice_of(&self.bookstore_categories, page, per_page)
                .iter()
                .enumerate()
                .flat_map(|(offset, category)| {
                    let index = page * per_page + offset;
                    let favorite = self.is_favorite(&category.id);
                    [
                        (
                            format!("{BOOKSTORE_CATEGORY}.{index}"),
                            self.row_title(&category.path),
                            bookstore_category_summary(category),
                            Glyph::Book,
                        ),
                        (
                            format!("{FAVORITE_CATEGORY}.{index}"),
                            if favorite {
                                "Remove from Discover".to_owned()
                            } else {
                                "Show on Discover".to_owned()
                            },
                            if favorite {
                                "Favorite category".to_owned()
                            } else {
                                "Quick access on Discover".to_owned()
                            },
                            Glyph::Bookmark,
                        ),
                    ]
                });
            screen = screen.rows(rows);
            screen = Self::page_controls(
                screen,
                Paging::sized(page, self.bookstore_categories.len(), per_page),
            );
        }
        screen.build()
    }

    /// One list of books: a discovery group, a subject, or an author.
    ///
    /// The list's name is the bar's title rather than a heading. A subject name
    /// arrives from the catalogue and "Enfant, adolescent et enseignement" is
    /// two lines of display type -- a row of books, spent saying again what the
    /// reader just tapped -- where the bar sets it on one line and ellipsises
    /// what does not fit. That is the renderer's own rule for a bar title, so
    /// the name is on one line by construction rather than by arithmetic here.
    fn browse_screen(&self) -> Screen {
        let mut screen = Self::nav(
            ScreenBuilder::new("browse")
                .top_bar(if self.browse.title.is_empty() {
                    "Books".to_owned()
                } else {
                    self.browse.title.clone()
                })
                .top_bar_action(BACK, "Back")
                .top_bar_action(FILTERS, "Filters"),
            0,
        );
        screen = self.note_block(screen, BannerLevel::Attention);
        // What the filters screen has been told, in one line, so that the
        // reader can see what is applied without opening it.
        if let Some(applied) = self.applied_filters() {
            screen = screen.secondary(applied);
        }
        if self.browsed.is_empty() && self.awaiting(RequestKind::Browse) {
            return screen.skeleton(self.skeleton_lines()).build();
        }
        if self.browsed.is_empty() {
            screen = screen.empty_state("No books in this list.");
        } else {
            let per_page = self.browse_rows_per_page();
            screen = self.publication_rows(
                screen,
                "browse",
                self.browse_offset * per_page,
                slice_of(&self.browsed, self.browse_offset, per_page),
            );
            screen = Self::page_controls(screen, self.browse_bounds());
        }
        screen.build()
    }

    /// How the list is ordered and what it leaves out.
    ///
    /// Every control here is drawn whatever the catalogue said. An order the
    /// catalogue did not offer is a sentence saying so, never a missing chip:
    /// the reader has to be able to see that there was nothing to choose from,
    /// and the PDF filter -- which is this app's, not the catalogue's -- has to
    /// be reachable either way.
    fn filters_screen(&self) -> Screen {
        let mut screen = Self::nav(
            ScreenBuilder::new("filters")
                .top_bar("Filters")
                .top_bar_action(BACK, "Done"),
            0,
        );
        screen = self.note_block(screen, BannerLevel::Info);
        let groups = self.filter_groups();
        let per_page = self.filter_groups_per_page(&groups);
        let page = self
            .filters_page
            .min(groups.len().saturating_sub(1) / per_page);
        for group in slice_of(&groups, page, per_page) {
            screen = screen.section(group.title);
            screen = if group.chips.is_empty() {
                screen.secondary(group.empty)
            } else {
                screen.chips(group.chips.iter().cloned())
            };
        }
        Self::page_controls(screen, Paging::sized(page, groups.len(), per_page)).build()
    }

    /// Books like the one being read about.
    ///
    /// The book's own title is the bar's, for the reason
    /// [`Self::browse_screen`] gives: a title arrives from the catalogue, and
    /// display type set across two lines of it costs a row of the list it is
    /// introducing.
    fn related_screen(&self) -> Screen {
        let title = self
            .detail
            .as_ref()
            .map_or_else(|| "This book".to_owned(), |book| book.title.clone());
        let mut screen = Self::nav(
            ScreenBuilder::new("related")
                .top_bar(format!("Like {title}"))
                .top_bar_action(BACK, "Back"),
            0,
        )
        .secondary("Same author, or the same kind of book, at either library.");
        screen = self.note_block(screen, BannerLevel::Attention);
        if self.related.is_empty() && self.awaiting(RequestKind::Related) {
            return screen.skeleton(self.skeleton_lines()).build();
        }
        if self.related.is_empty() {
            screen = screen.empty_state("Nothing close enough to suggest.");
        } else {
            let per_page = self.related_rows_per_page();
            let paging = Paging::sized(self.related_page, self.related.len(), per_page);
            screen = self.publication_rows(
                screen,
                "related",
                self.related_page * per_page,
                slice_of(&self.related, self.related_page, per_page),
            );
            screen = Self::page_controls(screen, paging);
        }
        screen.build()
    }

    /// The books the libraries are keeping for you.
    ///
    /// There is no cancel here. The catalogue says a hold can be cancelled but
    /// never says how, so this app does not guess at a request that would give
    /// a place in a queue away; the reader is told where it can be done.
    fn holds_screen(&self) -> Screen {
        let holds = self.filtered_holds();
        let mut screen = Self::nav(
            ScreenBuilder::new("holds")
                .top_bar("On hold")
                .top_bar_action(BACK, "Back"),
            1,
        )
        .heading("Waiting for you")
        .text("Your library tells you when one is ready. A hold can only be cancelled from your library account.");
        screen = self.note_block(screen, BannerLevel::Info);
        if holds.is_empty() {
            screen = screen.empty_state("You are not waiting for anything.");
        } else {
            let per_page = self.holds_rows_per_page();
            let paging = Paging::sized(self.holds_page, holds.len(), per_page);
            screen = screen.rows(
                slice_of(&holds, self.holds_page, per_page)
                    .iter()
                    .enumerate()
                    .map(|(offset, hold)| {
                        (
                            format!("hold.{}", self.holds_page * per_page + offset),
                            self.row_title(&hold.title),
                            hold_summary(hold),
                            Glyph::Bookmark,
                        )
                    }),
            );
            screen = Self::page_controls(screen, paging);
        }
        screen.build()
    }

    #[allow(clippy::too_many_lines)]
    fn detail_screen(&self) -> Screen {
        let Some(result) = self.detail.as_ref() else {
            return self.results_screen();
        };
        let mut screen = Self::nav(
            ScreenBuilder::new("detail")
                .top_bar("Book details")
                .top_bar_action(BACK, "Back")
                // Keep the two secondary book actions in the bar. A cover can
                // consume the optional rows on a Clara, but the full blurb
                // must remain one tap away even then.
                .compose(|mut screen| {
                    if self.cover.is_some() && result.description.is_some() {
                        screen = screen.top_bar_action(DESCRIPTION, "Description");
                    }
                    if result.related_handle().is_some() {
                        screen = screen.top_bar_action(RELATED, "Similar");
                    }
                    screen
                }),
            0,
        )
        .heading(result.title.clone());
        if let Some(cover) = self.cover {
            screen = screen.unframed_picture(cover, 24);
        }
        // The note goes above everything the screen might have to leave out.
        // A borrow that was never sent reports itself here, and the panel drops
        // whatever runs past the bottom without saying so.
        screen = self.note_block(self.watch_block(screen), BannerLevel::Attention);
        screen = screen.facts(self.detail_facts(result));
        // The tap is the borrow, so the row has to say so before it is taken.
        // A hold is offered in its place only when no library has a copy: while
        // one of them does, the book is available and joining a queue for it
        // would be a worse answer than borrowing it.
        let borrowable = result.is_available();
        screen = if borrowable {
            screen
                .section("Choose a library")
                .secondary("Tapping one borrows it, onto your home server.")
        } else {
            screen
                .section("Place a hold")
                .secondary("Every copy is out. Tapping a library puts you on its waiting list.")
        };
        screen = screen.rows(
            result
                .sources
                .iter()
                .enumerate()
                .map(|(source_index, source)| {
                    let status = availability_label(source);
                    let action = if source.available {
                        "Borrow & send"
                    } else if borrowable {
                        "Unavailable"
                    } else {
                        "Place a hold"
                    };
                    (
                        format!("source.{source_index}"),
                        source.catalog_name.clone(),
                        format!("{status} · {action}"),
                        Glyph::Globe,
                    )
                }),
        );
        // Everything below the libraries is optional, and drawn only as far as
        // the panel has room: the layout engine stops at the bottom of the
        // content area and drops the rest in silence, and what it would drop
        // here is the control the reader came for.
        let slots = self.detail_extras();
        // The first author only. A book with four of them has four rows on a
        // panel with room for one, and the first is the one a reader means.
        if slots >= 1 {
            if let Some(author) = result.authors.first() {
                screen = screen.section_rows(
                    "More by this author",
                    None,
                    [(
                        "author.0".to_owned(),
                        author.clone(),
                        "At either library".to_owned(),
                        Glyph::Circle,
                    )],
                );
            }
        }
        // The opening of the blurb, and the way into the rest of it. A row
        // rather than the paragraph this was, because a description runs to
        // thousands of characters and a book's screen has room for two lines of
        // them: what it can honestly offer is a taste and a tap.
        if let Some(description) = result.description.as_ref().filter(|_| slots >= 2) {
            screen = screen.section_rows(
                "About this book",
                None,
                [(
                    DESCRIPTION.to_owned(),
                    compact_message(description, MAX_DETAIL_DESCRIPTION_CHARS),
                    "Read the whole description".to_owned(),
                    Glyph::Book,
                )],
            );
        }
        screen.build()
    }

    /// What a book's screen says about the book, in the order it is worth
    /// saying, cut to what the panel holds.
    ///
    /// The order is the whole of the design here, because a six inch panel has
    /// room for three of these and the rest are dropped. The author and where a
    /// copy is are what a borrow cannot be made without. A PDF warning comes
    /// next, since it is the one fact that changes the answer. Then what a
    /// reader chooses on: whether other people liked it, and how much of it
    /// there is -- which for an audiobook is the fact the reader most wants.
    /// Then the edition, which is the part nobody has ever decided a borrow on.
    ///
    /// Cut from the end rather than drawn in full: what the layout engine does
    /// with a publisher's name that does not fit is drop the library button
    /// under it, and say nothing.
    fn detail_facts(&self, book: &Publication) -> Vec<(&'static str, String)> {
        let rating = book.goodreads_rating.map(|rating| {
            let ratings = book.goodreads_ratings_count.map_or_else(
                || "ratings not counted".to_owned(),
                |count| format!("{} ratings", count_label(count)),
            );
            format!("{rating:.1} / 5 · {ratings}")
        });
        [
            ("Author", Some(author_line(&book.authors))),
            ("Copies", Some(availability_summary(book))),
            ("Length", length_label(book)),
            (
                "Published",
                book.published.as_deref().and_then(published_label),
            ),
            ("Les Libraires", self.bookstore_category.clone()),
            (
                "Format",
                book.pdf_only
                    .then(|| "PDF only, hard to read here".to_owned()),
            ),
            ("Goodreads", rating),
            ("Publisher", book.publisher.clone()),
        ]
        .into_iter()
        .filter_map(|(label, value)| value.map(|value| (label, value)))
        .take(self.detail_fact_rows(book.sources.len()))
        .collect()
    }

    /// One book's blurb, with the panel to itself.
    ///
    /// No heading of its own. The bar already says what this is, and a title
    /// set across two lines above the words costs a fifth of what there is to
    /// read them in.
    fn description_screen(&self) -> Screen {
        let pages = self.description_pages();
        let mut screen = Self::nav(
            ScreenBuilder::new("description")
                .top_bar("About this book")
                .top_bar_action(BACK, "Back"),
            0,
        );
        screen = self.note_block(screen, BannerLevel::Attention);
        let showing = self.description_page.min(pages.len().saturating_sub(1));
        let Some(page) = pages.get(showing) else {
            return screen
                .empty_state("Neither library describes this book.")
                .build();
        };
        // Asked for its first page, because the paragraphs handed over are
        // already the page: this screen decides where the folds are, since only
        // it knows what the controls under them cost.
        screen = screen.paged_list(0, page.clone());
        screen = Self::page_controls(screen, Paging::sized(showing, pages.len(), 1));
        screen.build()
    }

    fn library_screen(&self) -> Screen {
        let mut screen = Self::nav(ScreenBuilder::new("library").top_bar("My books"), 1)
            // "Both", not "All": three characters do not reach the renderer's
            // minimum touch target, and it matches the Search screen anyway.
            .chips([
                (FILTER_ALL, "Both", self.filter == CatalogFilter::All),
                (
                    FILTER_MONTREAL,
                    "Montréal",
                    self.filter == CatalogFilter::Montreal,
                ),
                (FILTER_BANQ, "BAnQ", self.filter == CatalogFilter::Banq),
            ])
            .top_bar_action(REFRESH, "Refresh");
        if self.books.is_empty() && self.awaiting(RequestKind::Books) {
            return screen.skeleton(self.skeleton_lines()).build();
        }
        screen = self.note_block(self.watch_block(screen), BannerLevel::Attention);
        let unresolved = self.unresolved_jobs();
        if !unresolved.is_empty() {
            screen = screen.section_rows(
                "Needs your attention",
                None,
                unresolved
                    .iter()
                    .take(self.attention_rows())
                    .filter_map(|&index| {
                        let job = self.jobs.get(index)?;
                        Some((
                            format!("resolve.{index}"),
                            self.row_title(&job.title),
                            unresolved_summary(job),
                            Glyph::Circle,
                        ))
                    }),
            );
        }
        // Holds are a queue rather than a shelf, so they get their own screen
        // and one row here saying how many there are. A single paged list per
        // screen is also the only way Previous and Next can mean one thing.
        let holds = self.filtered_holds();
        if !holds.is_empty() {
            screen = screen.section_rows(
                "On hold",
                None,
                [(
                    HOLDS,
                    "Waiting for you".to_owned(),
                    holds_summary(&holds),
                    Glyph::Bookmark,
                )],
            );
        }
        // A loan the library has but the home server does not: borrowed
        // somewhere else. There is nothing here to return, so it is a sentence
        // rather than a row that leads nowhere.
        let elsewhere = self.unheld_loans();
        if elsewhere > 0 {
            screen = screen.secondary(match elsewhere {
                1 => "Your library also has one loan that is not on your home server.".to_owned(),
                many => {
                    format!("Your library also has {many} loans that are not on your home server.")
                }
            });
        }
        let books = self.filtered_books();
        if books.is_empty() {
            screen = screen.empty_state("No loans from this library.");
        } else {
            let per_page = self.library_rows_per_page();
            let paging = Paging::sized(self.books_page, books.len(), per_page);
            let base = self.books_page * per_page;
            screen = screen.rows(
                slice_of(&books, self.books_page, per_page)
                    .iter()
                    .enumerate()
                    .map(|(offset, book)| {
                        (
                            format!("book.{}", base + offset),
                            self.row_title(&book.title),
                            self.book_summary(book),
                            Glyph::Bookmark,
                        )
                    }),
            );
            if paging.has_previous || paging.has_next {
                screen = Self::page_controls(screen, paging);
            }
        }
        screen.build()
    }

    fn confirm_return_screen(&self) -> Screen {
        let Some(index) = self.selected_book else {
            return self.library_screen();
        };
        let books = self.filtered_books();
        let Some(book) = books.get(index) else {
            return self.library_screen();
        };
        Self::nav(
            ScreenBuilder::new("confirm-return")
                .top_bar("Return loan")
                .top_bar_action(CANCEL, "Cancel"),
            1,
        )
        .heading("Return this book?")
        .text(format!(
            "{}\n{}\n\nThe loan stays on your home server until {} confirms the return.",
            book.title,
            catalog_label(&book.catalog),
            catalog_label(&book.catalog)
        ))
        .buttons([(CONFIRM_RETURN, "Return loan"), (CANCEL, "Keep loan")])
        .build()
    }

    /// The one screen that asks the reader for a decision.
    ///
    /// Acknowledging is the only way past a borrow or return the server refuses
    /// to guess about, so it is a labelled button behind an explanation rather
    /// than a cross on a row: the reader is being asked to confirm they looked
    /// at their library account, not to hide a message.
    fn resolve_screen(&self) -> Screen {
        let Some(job) = self.resolve_target() else {
            return self.library_screen();
        };
        let mut screen = Self::nav(
            ScreenBuilder::new("resolve")
                .top_bar("Needs you")
                .top_bar_action(BACK, "Back"),
            1,
        )
        .heading(job.title.clone())
        .text(resolve_explanation(job));
        screen = self.note_block(screen, BannerLevel::Attention);
        if let Some(error) = &job.error {
            screen = screen
                .section("What the home server said")
                .secondary(compact_message(error, MAX_ERROR_CHARS));
        }
        // The decision, not the app: a list being read somewhere else must not
        // take the buttons away from the one screen that asks for one.
        screen = if self.deciding() {
            screen.activity("Telling the home server...", None)
        } else if job.state == "hook_failed" {
            screen.buttons([(RETRY_HOOK, "Send to my reader again"), (CANCEL, "Not now")])
        } else {
            screen.buttons([(ACKNOWLEDGE, "I checked my account"), (CANCEL, "Not now")])
        };
        screen.build()
    }

    fn settings_screen(&self) -> Screen {
        let mut screen = Self::nav(ScreenBuilder::new("settings").top_bar("Settings"), 2)
            .heading("Connection")
            .facts([
                ("Libraries", self.health.as_deref().unwrap_or("Not checked")),
                ("Book files", "Home server only"),
                ("Library passwords", "Never sent to Kobo"),
            ]);
        if let Some(advice) = &self.health_advice {
            screen = screen.section("Sign-in needed").text(advice.clone());
        }
        screen = self
            .note_block(screen, BannerLevel::Info)
            .button(REFRESH, "Check connection");
        screen.build()
    }

    fn filtered_books(&self) -> Vec<Book> {
        self.books
            .iter()
            .filter(|book| self.filter.matches(&book.catalog))
            .cloned()
            .collect()
    }

    /// Makes room for something the reader just asked the server to do.
    ///
    /// A list being read is dropped: it is idempotent, nothing has been told to
    /// change, and it can be asked for again. Something already changing on the
    /// server has to finish, and false says so, because two borrows or two
    /// returns of the same book are not the same as one.
    fn clear_for_request(&mut self, context: &mut Context) -> bool {
        match self.inflight {
            Some((task, kind)) if is_read(kind) => {
                context.cancel(task);
                self.inflight = None;
                true
            }
            Some(_) => false,
            None => true,
        }
    }

    /// How many things Library names in one go as needing a person.
    ///
    /// Past that the section takes the whole panel and the loans under it are
    /// the rows the layout engine drops. What is left is still reported: the
    /// section is read again on every refresh, and one settled decision brings
    /// the next into view.
    fn attention_rows(&self) -> usize {
        if self.page_rows() >= 5 {
            2
        } else {
            1
        }
    }

    /// One page of a list of books.
    fn publication_rows(
        &self,
        screen: ScreenBuilder,
        prefix: &str,
        base: usize,
        publications: &[Publication],
    ) -> ScreenBuilder {
        screen.rows(
            publications
                .iter()
                .enumerate()
                .map(|(offset, publication)| {
                    (
                        format!("{prefix}.{}", base + offset),
                        self.row_title(&publication.title),
                        publication_summary(publication),
                        Glyph::Book,
                    )
                }),
        )
    }

    /// A wait drawn at the height of the page it is standing in for.
    fn skeleton_lines(&self) -> u8 {
        u8::try_from(self.page_rows()).unwrap_or(3)
    }

    /// How tall this panel's content area is -- the screen's, not the panel's.
    ///
    /// `prose_area` describes the panel. It knows about the top bar and the
    /// navigation bar because this app asked for both, and nothing at all about
    /// the strip carrying the clock, the radio and the battery, which the
    /// runtime lays out above everything else and never tells an application
    /// about. So it comes off here.
    ///
    /// Fifty-nine pixels on a Clara. Every budget below was written against the
    /// panel rather than the screen and was therefore that much too generous on
    /// every real device, while the simulator and the layout tests -- neither of
    /// which drew the band -- agreed with the arithmetic and passed. The browse
    /// page controls were the first thing to be sheared off by the bar, and the
    /// book screen's last row went with them.
    fn content_height(&self) -> i32 {
        (self.metrics.prose_area(true, true).height - self.metrics.status_band_height()).max(1)
    }

    /// The width a row's words are set across, which is not the panel's.
    ///
    /// A row reserves the lead column whether or not it has a lead, so that a
    /// list of rows with mixed leads keeps one text margin.
    fn row_width(&self) -> i32 {
        kobo_ui::row_text_width(&self.metrics, self.metrics.prose_area(true, true))
    }

    /// A row's title, on one line, ellipsised where it did not fit.
    ///
    /// This is what lets [`Self::list_rows`] measure one row and know every
    /// row. A title that wraps makes its row a fifth taller than the one above
    /// it, so a page counted from a one-line row and drawn with two wrapped
    /// titles overflows -- and a list whose rows all differ in height is one
    /// the eye has to re-measure on every line anyway.
    fn row_title(&self, title: &str) -> String {
        kobo_ui::one_line(title, self.row_width(), kobo_ui::FontSize::Body)
    }

    /// One gap: the step everything on a screen is spaced by.
    fn gap(&self) -> i32 {
        self.metrics.space(Space::Tight)
    }

    /// What a paged screen spends under its last row.
    ///
    /// A gap, the line that says which page this is, another gap, and the band
    /// that turns it. Reserved before the rows are counted, because a page
    /// measured without it comes back one row too many and that row is drawn
    /// under the controls and clipped by the bar.
    fn page_tail(&self) -> i32 {
        2 * self.gap()
            + kobo_ui::FontSize::Caption.line_height()
            + self
                .metrics
                .touch_target_default()
                .max(self.metrics.touch_target_minimum())
    }

    /// What one screen heading costs the list under it.
    fn heading_cost(&self) -> i32 {
        kobo_ui::FontSize::for_heading_level(1).line_height() + self.gap()
    }

    /// What one line of small print costs, the gap under it included.
    fn line_cost(&self) -> i32 {
        kobo_ui::FontSize::Caption.line_height() + self.gap()
    }

    /// What one line of body text costs. No gap: a block of prose is charged as
    /// its lines plus one gap, not as a gap per line.
    fn text_line_cost() -> i32 {
        kobo_ui::FontSize::Body.line_height()
    }

    /// What one section header costs, including the air a section is given
    /// above and below it.
    fn section_cost(&self) -> i32 {
        kobo_ui::FontSize::Caption.line_height() + 2 * self.gap()
    }

    /// What one *row* of chips costs. A run of them is charged by the row,
    /// because the renderer wraps them: four French facet names are two rows on
    /// a six inch panel, and the row the arithmetic did not charge for was the
    /// one that pushed the page controls under the bar.
    fn chip_row_cost(&self) -> i32 {
        self.metrics
            .touch_target_default()
            .max(self.metrics.touch_target_minimum())
            + self.gap()
    }

    /// How many rows a wrapped run of chips is laid out on.
    ///
    /// The renderer's own arm, followed step for step: a chip is as wide as its
    /// label and two pads, it is never wider than the content, and it starts a
    /// new row when it will not fit on this one. Guessing instead is how the
    /// browse screen came to be a line taller than its budget said.
    fn chip_rows(&self, labels: &[String]) -> i32 {
        let width = self.metrics.prose_area(true, true).width;
        let pad = self.metrics.space(Space::Small);
        let gap = self.gap();
        let mut rows = 1;
        let mut used = 0;
        for label in labels.iter().take(kobo_sdk::MAX_CHIPS) {
            let label =
                kobo_ui::one_line(label, (width - pad * 2).max(1), kobo_ui::FontSize::Caption);
            let chip =
                (kobo_ui::measure_text(&label, kobo_ui::FontSize::Caption).0 + pad * 2).min(width);
            if used > 0 && used + chip > width {
                rows += 1;
                used = 0;
            }
            used += chip + gap;
        }
        rows
    }

    /// How many rows fit in what is left of the screen after `spent`.
    ///
    /// Asked of the renderer rather than worked out here. A row's height and
    /// the separator between two of them are the panel's business, every panel
    /// answers differently, and the app that wrote the answer down was wrong on
    /// two of the three it draws to. Every row on a paged screen carries one
    /// line of title and one line of summary -- see [`Self::row_title`] -- so
    /// measuring a sample of them measures all of them.
    ///
    /// `want` is asked for rather than assumed because a screen with sections
    /// above its list has rows above its list: charging those as rows is the
    /// only way to charge them at what they cost, separators included.
    fn rows_that_fit(&self, spent: i32, want: usize) -> usize {
        let mut area = self.metrics.prose_area(true, true);
        area.height = (self.content_height() - self.page_tail() - spent).max(1);
        let sample = vec![("One line of title", "One line of summary"); want.max(1)];
        kobo_ui::paginate_rows(&sample, &self.metrics, area)
            .first()
            .map_or(1, Vec::len)
    }

    /// How many rows of a list fit under `spent` pixels of whatever the screen
    /// puts above it.
    fn list_rows(&self, spent: i32) -> usize {
        self.rows_that_fit(spent, MAX_PAGE_ROWS)
            .clamp(MIN_PAGE_ROWS, MAX_PAGE_ROWS)
    }

    /// How many rows a list under a screen heading holds on one page.
    fn page_rows(&self) -> usize {
        self.list_rows(self.heading_cost())
    }

    /// How many books a discovery list shows before offering the rest of it.
    /// The section's own heading and the row that opens the whole list come off
    /// the page first.
    fn discover_books(&self) -> usize {
        self.page_rows().saturating_sub(2).max(1)
    }

    /// How many of a book's optional blocks fit under the libraries.
    ///
    /// Nothing while there is a request to follow or a failure to read: a
    /// reader watching a borrow is not looking for a different book. One on a
    /// panel the size of a Nia, which has room for the ways on from the book
    /// but not for a paragraph as well. Both on anything larger.
    fn detail_extras(&self) -> usize {
        let busy = self.note.is_some()
            || self
                .watch
                .as_ref()
                .is_some_and(|watch| watch.view == View::Detail);
        if busy {
            0
        } else if self.cover.is_some() {
            usize::from(self.content_height() >= 1600)
        } else if self.content_height() >= TALL_PANEL {
            2
        } else {
            1
        }
    }

    /// How many facts fit between a book's title and the libraries under it.
    ///
    /// What is left of the panel after the ways on from the book, the section
    /// that offers the libraries and a row for each of them, at the cost a
    /// definition list charges for a line. The block grew from four entries to
    /// seven when the server started sending the edition, and seven of them on
    /// a six inch panel is the borrow button pushed off the bottom.
    ///
    /// The libraries are counted rather than assumed, because their rows are
    /// half of what the screen spends: a book only one library carries has room
    /// for four facts where a book both of them carry has room for two.
    ///
    /// The gap given back is the one the last fact does not pay for, and it is
    /// worth a whole fact on a six inch panel: without it the arithmetic said
    /// one fact where two were drawn perfectly well, and the answer had to be
    /// rescued by [`MIN_DETAIL_FACTS`] instead of being right.
    fn detail_fact_rows(&self, libraries: usize) -> usize {
        let extras = i32::try_from(self.detail_extras()).unwrap_or(0);
        let blurb = if extras >= 2 { BLURB_EXTRA_LINE } else { 0 };
        let cover = self
            .cover
            .map(|_| self.metrics.tenth_mm(240) + self.gap())
            .unwrap_or(0);
        let libraries = i32::try_from(libraries).unwrap_or(2).max(1);
        let room = self.content_height()
            - DETAIL_FURNITURE
            - cover
            - libraries * LIBRARY_ROW_COST
            - extras * DETAIL_EXTRA_COST
            - blurb;
        usize::try_from((room + self.gap()) / FACT_COST)
            .unwrap_or(MIN_DETAIL_FACTS)
            .max(MIN_DETAIL_FACTS)
    }

    /// Where the folds fall in the open book's blurb.
    ///
    /// Measured against the panel rather than counted in characters, because a
    /// page of one long paragraph holds half again what a page of short ones
    /// does. The room is the screen's content area -- which already excludes the
    /// strip the runtime keeps for the clock -- less the line that says which
    /// page this is and the band that turns it. Measuring without those comes
    /// back a page too full, and the layout engine drops the overflow in
    /// silence.
    fn description_pages(&self) -> Vec<Vec<String>> {
        let Some(description) = self
            .detail
            .as_ref()
            .and_then(|book| book.description.as_deref())
        else {
            return Vec::new();
        };
        let mut area = self.metrics.prose_area(true, true);
        let mut spent = self.page_tail();
        if self.note.is_some() {
            spent += NOTE_LINES * self.line_cost() + self.gap();
        }
        area.height = (self.content_height() - spent).max(1);
        kobo_ui::paginate(description, area)
    }

    /// Whether the thing this screen is for is the thing being waited on.
    ///
    /// One flag for "busy" was wrong once there was more than one list: reading
    /// the home page made the loans look as though they were loading, and the
    /// screen that asks a reader for a decision took its own buttons away.
    fn awaiting(&self, kind: RequestKind) -> bool {
        self.inflight
            .is_some_and(|(_, in_flight)| in_flight == kind)
            || self.queued_request == Some(kind)
    }

    /// Whether the reader's own decision is the thing in flight.
    fn deciding(&self) -> bool {
        self.awaiting(RequestKind::RetryHook) || self.awaiting(RequestKind::Acknowledge)
    }

    fn filtered_holds(&self) -> Vec<ShelfEntry> {
        self.shelf
            .iter()
            .filter(|entry| entry.is_hold() && self.filter.matches(&entry.catalog))
            .cloned()
            .collect()
    }

    fn is_favorite(&self, id: &str) -> bool {
        self.favorites.iter().any(|favorite| favorite == id)
    }

    fn favorite_categories(&self) -> Vec<BookstoreCategory> {
        self.favorites
            .iter()
            .filter_map(|id| {
                self.bookstore_categories
                    .iter()
                    .find(|category| &category.id == id)
            })
            .cloned()
            .collect()
    }

    fn bookstore_categories_rows_per_page(&self) -> usize {
        let spent = self.heading_cost() + 2 * Self::text_line_cost() + self.gap();
        (self.rows_that_fit(spent, MAX_PAGE_ROWS) / 2).max(1)
    }

    fn toggle_favorite(&mut self, context: &mut Context, index: usize) {
        let Some(category) = self.bookstore_categories.get(index) else {
            return;
        };
        if let Some(position) = self.favorites.iter().position(|id| id == &category.id) {
            self.favorites.remove(position);
        } else {
            self.favorites.push(category.id.clone());
        }
        self.save_state(context);
        self.show(context);
    }

    /// What the library says about this loan, matched to what the home server
    /// has.
    ///
    /// Matched on the library and the title, because the two lists are answers
    /// from two different systems and only the book is common to both. The
    /// server's own identifier means nothing to the catalogue.
    fn shelf_loan(&self, book: &Book) -> Option<&ShelfEntry> {
        self.shelf.iter().find(|entry| {
            entry.is_loan()
                && entry.catalog == book.catalog
                && same_title(&entry.title, &book.title)
        })
    }

    /// The loans the library says are out that the home server has no file for
    /// -- borrowed somewhere else, most likely on the library's own site.
    fn unheld_loans(&self) -> usize {
        self.shelf
            .iter()
            .filter(|entry| entry.is_loan() && self.filter.matches(&entry.catalog))
            .filter(|entry| {
                !self.books.iter().any(|book| {
                    book.catalog == entry.catalog && same_title(&entry.title, &book.title)
                })
            })
            .count()
    }

    /// How many loans fit under whatever else Library has to say today.
    ///
    /// The sections above the list are conditional, so a fixed page height
    /// would push the last loans off the panel on exactly the days something
    /// needs a person. The list gets what is left.
    ///
    /// The rows those sections carry are charged as rows rather than as pixels,
    /// because that is what they are: a section's row costs the same separator
    /// as a loan's, and the run of filter chips at the top of this screen was
    /// never charged for at all.
    fn library_rows_per_page(&self) -> usize {
        let mut spent = self.chip_row_cost();
        let mut above = 0;
        let unresolved = self.unresolved_jobs().len().min(self.attention_rows());
        if unresolved > 0 {
            spent += self.section_cost();
            above += unresolved;
        }
        if !self.filtered_holds().is_empty() {
            spent += self.section_cost();
            above += 1;
        }
        if self.unheld_loans() > 0 {
            spent += self.line_cost();
        }
        self.rows_that_fit(spent, above + MAX_PAGE_ROWS)
            .saturating_sub(above)
            .clamp(1, MAX_PAGE_ROWS)
    }

    /// How many books a browsed page shows.
    ///
    /// Its name is in the bar rather than in a heading, so a browse page is the
    /// list and the controls that turn it and nothing else -- less the one line
    /// that reports what the filters screen has been told, when it has been
    /// told anything.
    fn browse_rows_per_page(&self) -> usize {
        self.list_rows(if self.applied_filters().is_some() {
            self.line_cost()
        } else {
            0
        })
    }

    /// How many titles a page of search results shows: a heading and the line
    /// repeating what was searched for come off it first.
    fn results_rows_per_page(&self) -> usize {
        self.list_rows(self.heading_cost() + Self::text_line_cost() + self.gap())
    }

    /// How many neighbours a page shows. The book's own title is in the bar, so
    /// only the line explaining what "like this" means comes off the page.
    fn related_rows_per_page(&self) -> usize {
        self.list_rows(self.line_cost())
    }

    /// How many queued books a page shows, under the heading and the two lines
    /// saying where a hold can be cancelled.
    fn holds_rows_per_page(&self) -> usize {
        self.list_rows(self.heading_cost() + 2 * Self::text_line_cost() + self.gap())
            .max(2)
    }

    /// Which way a browse can still be turned.
    ///
    /// The next page is the server's answer, never a count of the rows in hand:
    /// a merged list can end on a full page, and a client that guessed from the
    /// count would offer a page that is not there.
    fn browse_bounds(&self) -> Paging {
        Paging {
            page: self.browse_number,
            total_pages: None,
            has_previous: self.browse_offset > 0 || self.browse_paging.has_previous,
            has_next: (self.browse_offset + 1) * self.browse_rows_per_page() < self.browsed.len()
                || self.browse_paging.has_next,
        }
    }

    /// Everything the filters screen offers, as data.
    ///
    /// A list rather than two hard-coded controls, because the catalogue also
    /// takes an audience, an availability, a language and a nature, and a later
    /// wave adds them. Adding one is adding an entry here; the screen already
    /// knows how to page what it cannot fit.
    ///
    /// One limit is upstream and permanent: two subjects are not combinable --
    /// the catalogue answers the second one and ignores the first -- so a
    /// subject is never a group here.
    fn filter_groups(&self) -> Vec<FilterGroup> {
        vec![
            FilterGroup {
                title: "Order",
                empty: "This list comes in one order only.",
                chips: self
                    .sorts
                    .iter()
                    .enumerate()
                    .map(|(index, sort)| {
                        (
                            format!("sort.{index}"),
                            sort.label.clone(),
                            self.browse.sort.as_deref() == Some(sort.key.as_str()),
                        )
                    })
                    .collect(),
            },
            FilterGroup {
                title: "Format",
                empty: "",
                // Never conditional on anything the server said. A PDF on a six
                // inch panel is a photograph of a paper page, so this is the
                // app's own filter, and the reader has to be able to reach it
                // whether or not the catalogue offered any orders at all.
                chips: vec![(SHOW_PDF.to_owned(), "Include PDF".to_owned(), self.show_pdf)],
            },
            FilterGroup {
                title: "Quick lists",
                empty: "No curated lists are configured.",
                chips: Self::browse_presets()
                    .iter()
                    .enumerate()
                    .map(|(index, (_, label, _, _, _, _))| {
                        (
                            format!("{PRESET}.{index}"),
                            (*label).to_owned(),
                            self.preset_selected(index),
                        )
                    })
                    .collect(),
            },
        ]
    }

    /// Curated combinations of the same query facets the catalog exposes.
    /// They are deliberately named rather than making a reader remember URL
    /// syntax, and the proxy still applies the final filtering upstream.
    fn browse_presets() -> [(
        &'static str,
        &'static str,
        Option<&'static str>,
        Option<&'static str>,
        bool,
        Option<&'static str>,
    ); 2] {
        [
            (
                "12-plus-available",
                "12+ · Available fiction",
                Some("F"),
                Some("12_and_over"),
                true,
                Some("issued_on_desc"),
            ),
            (
                "young-fiction",
                "Young adult fiction",
                Some("YFB"),
                None,
                false,
                None,
            ),
        ]
    }

    fn preset_selected(&self, index: usize) -> bool {
        let presets = Self::browse_presets();
        let Some((_, _, category, audience, available, sort)) = presets.get(index) else {
            return false;
        };
        !self.browse.bookstore
            && self.browse.category.as_deref() == *category
            && self.browse.audience.as_deref() == *audience
            && self.browse.available_only == *available
            && self.browse.sort.as_deref() == *sort
    }

    /// How many filter groups fit on one page of the filters screen.
    fn filter_groups_per_page(&self, groups: &[FilterGroup]) -> usize {
        let room = self.content_height() - self.page_tail();
        let mut taken = 0;
        let mut fit = 0;
        for group in groups {
            let labels = group
                .chips
                .iter()
                .map(|(_, label, _)| label.clone())
                .collect::<Vec<_>>();
            let body = if labels.is_empty() {
                self.line_cost()
            } else {
                self.chip_rows(&labels) * self.chip_row_cost()
            };
            taken += self.section_cost() + body;
            if taken > room && fit > 0 {
                break;
            }
            fit += 1;
        }
        fit.max(1)
    }

    /// What the filters screen has been told, in one line, or nothing when it
    /// has been told nothing.
    ///
    /// Only what differs from the plain list: a reader who has chosen nothing
    /// should not be reading a line about it, and the line costs the list a row
    /// once it is there.
    fn applied_filters(&self) -> Option<String> {
        let mut parts = Vec::new();
        if let Some(sort) = self.browse.sort.as_deref() {
            if let Some(label) = self
                .sorts
                .iter()
                .find(|candidate| candidate.key == sort)
                .map(|candidate| candidate.label.clone())
            {
                parts.push(label);
            }
        }
        if self.show_pdf {
            parts.push("PDF included".to_owned());
        }
        if self.browse.available_only {
            parts.push("Available now".to_owned());
        }
        if let Some(audience) = self.browse.audience.as_deref() {
            parts.push(match audience {
                "12_and_over" => "12+".to_owned(),
                other => other.replace('_', " "),
            });
        }
        if parts.is_empty() {
            return None;
        }
        Some(kobo_ui::one_line(
            &parts.join(" · "),
            self.metrics.prose_area(true, true).width,
            kobo_ui::FontSize::Caption,
        ))
    }

    fn resolve_target(&self) -> Option<&Job> {
        let id = self.resolve_job.as_deref()?;
        self.jobs.iter().find(|job| job.id == id)
    }

    /// The unacknowledged job that explains this loan's row, if there is one.
    fn book_issue(&self, book: &Book) -> Option<&Job> {
        self.jobs.iter().find(|job| {
            job.book_id.as_deref() == Some(book.id.as_str())
                && matches!(job.state.as_str(), "hook_failed" | "return_uncertain")
        })
    }

    /// The settled requests a person has to decide about that have no loan of
    /// their own to sit against. Empty is the normal case, and an empty list
    /// draws nothing at all.
    fn unresolved_jobs(&self) -> Vec<usize> {
        self.jobs
            .iter()
            .enumerate()
            .filter(|(_, job)| self.filter.matches(&job.catalog))
            .filter(|(_, job)| match job.state.as_str() {
                "borrow_uncertain" | "hold_uncertain" | "failed" => true,
                // A delivery failure belongs on its loan. Without one there is
                // nowhere else for it to go.
                "hook_failed" => !self
                    .books
                    .iter()
                    .any(|book| Some(book.id.as_str()) == job.book_id.as_deref()),
                _ => false,
            })
            .map(|(index, _)| index)
            .collect()
    }

    fn book_summary(&self, book: &Book) -> String {
        let catalog = catalog_label(&book.catalog);
        if self
            .book_issue(book)
            .is_some_and(|job| job.state == "hook_failed")
        {
            return format!("{catalog} · Not sent to your reader");
        }
        if let Some(state) = book.return_state.as_deref() {
            return format!("{catalog} · {}", state_label(state));
        }
        match self
            .shelf_loan(book)
            .and_then(|entry| entry.until.as_deref())
            .and_then(plain_date)
        {
            Some(due) => format!("{catalog} · Due {due}"),
            None => format!("{catalog} · Ready to return"),
        }
    }

    fn start_search(&mut self, context: &mut Context) {
        if !self.clear_for_request(context) {
            self.queued_request = Some(RequestKind::Search);
            return;
        }
        let query: String = self.query.trim().chars().take(MAX_QUERY_CHARS).collect();
        if query.is_empty() {
            self.note = Some("Enter a title, author, or ISBN.".to_owned());
            self.view = View::Search;
            self.show(context);
            return;
        }
        query.clone_into(&mut self.query);
        let catalogs = Value::Array(
            self.filter
                .catalog_values()
                .into_iter()
                .map(|catalog| Value::String(catalog.to_owned()))
                .collect(),
        );
        let body = ObjectBuilder::new()
            .set("query", query)
            .set("catalogs", catalogs)
            .set("include_pdf", self.show_pdf)
            .build()
            .to_json();
        self.note = None;
        self.results_page = 0;
        if self.view != View::Results {
            self.trail.push(self.here());
        }
        self.view = View::Results;
        let task = Task::Post {
            url: format!("{API}/search"),
            body,
            content_type: "application/json".to_owned(),
            credential: Some(Self::credential()),
            headers: Vec::new(),
            max_bytes: 192 * 1024,
        };
        if let Some(task_id) = context.spawn_retrying(task) {
            self.inflight = Some((task_id, RequestKind::Search));
        } else {
            self.note = Some("The Kobo runtime is busy. Try again in a moment.".to_owned());
        }
        self.show(context);
    }

    fn start_books(&mut self, context: &mut Context) {
        if self.inflight.is_some() {
            self.queued_request = Some(RequestKind::Books);
            self.show(context);
            return;
        }
        let mut url = format!("{API}/books");
        if self.filter != CatalogFilter::All {
            url.push_str("?catalog=");
            url.push_str(match self.filter {
                CatalogFilter::Montreal => "montreal",
                CatalogFilter::Banq => "banq",
                CatalogFilter::All => "",
            });
        }
        let task = Task::Fetch {
            url,
            offset: 0,
            max_bytes: 96 * 1024,
            credential: Some(Self::credential()),
            headers: vec![Header::new("Accept", "application/json")],
        };
        if let Some(task_id) = context.spawn_retrying(task) {
            self.inflight = Some((task_id, RequestKind::Books));
        } else {
            self.note = Some("The Kobo runtime is busy. Try again in a moment.".to_owned());
        }
        self.show(context);
    }

    /// Reads the settled requests that still need a person.
    ///
    /// The server owns the durable list, so this is fetched rather than
    /// remembered; nothing about it is kept on the reader between launches.
    fn start_jobs(&mut self, context: &mut Context) {
        if self.inflight.is_some() {
            self.queued_request = Some(RequestKind::Jobs);
            self.show(context);
            return;
        }
        let task = Task::Fetch {
            url: format!("{API}/jobs?limit={MAX_JOBS}"),
            offset: 0,
            max_bytes: 128 * 1024,
            credential: Some(Self::credential()),
            headers: vec![Header::new("Accept", "application/json")],
        };
        if let Some(task_id) = context.spawn_retrying(task) {
            self.inflight = Some((task_id, RequestKind::Jobs));
        } else {
            self.note = Some("The Kobo runtime is busy. Try again in a moment.".to_owned());
        }
        self.show(context);
    }

    fn start_health(&mut self, context: &mut Context) {
        if self.inflight.is_some() {
            self.show(context);
            return;
        }
        let task = Task::Fetch {
            url: format!("{API}/health"),
            offset: 0,
            max_bytes: 32 * 1024,
            credential: Some(Self::credential()),
            headers: vec![Header::new("Accept", "application/json")],
        };
        if let Some(task_id) = context.spawn_retrying(task) {
            self.inflight = Some((task_id, RequestKind::Health));
        }
        self.show(context);
    }

    /// One retrying read, drawn wherever the reader is waiting for it.
    ///
    /// A read already in flight is dropped rather than waited for: the reader
    /// has just asked for something else, and the answer to the list they left
    /// is of no use to them. Anything that changes something on the server has
    /// to finish, so this waits behind that instead.
    fn start_read(&mut self, context: &mut Context, kind: RequestKind, url: String, cap: u32) {
        if !self.clear_for_request(context) {
            self.queued_request = Some(kind);
            self.show(context);
            return;
        }
        let task = Task::Fetch {
            url,
            offset: 0,
            max_bytes: cap,
            credential: Some(Self::credential()),
            headers: vec![Header::new("Accept", "application/json")],
        };
        if let Some(task_id) = context.spawn_retrying(task) {
            self.inflight = Some((task_id, kind));
        } else {
            self.note = Some("The Kobo runtime is busy. Try again in a moment.".to_owned());
        }
        self.show(context);
    }

    fn start_discovery(&mut self, context: &mut Context) {
        self.start_read(
            context,
            RequestKind::Discovery,
            format!("{API}/discovery"),
            256 * 1024,
        );
    }

    fn start_categories(&mut self, context: &mut Context) {
        self.start_read(
            context,
            RequestKind::CategoryList,
            format!("{API}/categories"),
            64 * 1024,
        );
    }

    fn start_bookstore_categories(&mut self, context: &mut Context) {
        self.start_read(
            context,
            RequestKind::BookstoreCategories,
            format!("{API}/bookstore/categories"),
            128 * 1024,
        );
    }

    fn start_browse(&mut self, context: &mut Context) {
        let endpoint = if self.browse.bookstore {
            "/bookstore/browse"
        } else {
            "/browse"
        };
        let mut url = format!("{API}{endpoint}?page={}", self.browse.page.max(1));
        if let Some(category) = &self.browse.category {
            url.push_str("&category=");
            url.push_str(&percent_encode(category));
        }
        if let Some(audience) = &self.browse.audience {
            url.push_str("&audience=");
            url.push_str(&percent_encode(audience));
        }
        if self.browse.available_only {
            url.push_str("&availability=available");
        }
        if let Some(author) = &self.browse.author {
            url.push_str("&author=");
            url.push_str(&percent_encode(author));
        }
        if let Some(sort) = &self.browse.sort {
            url.push_str("&sort=");
            url.push_str(&percent_encode(sort));
        }
        if self.show_pdf {
            url.push_str("&include_pdf=1");
        }
        self.start_read(context, RequestKind::Browse, url, 192 * 1024);
    }

    fn start_related(&mut self, context: &mut Context) {
        let Some(handle) = self
            .detail
            .as_ref()
            .and_then(Publication::related_handle)
            .map(percent_encode)
        else {
            return;
        };
        self.related.clear();
        self.related_page = 0;
        self.start_read(
            context,
            RequestKind::Related,
            format!("{API}/publications/{handle}/related"),
            128 * 1024,
        );
    }

    /// Reads what the libraries themselves say is out and waiting.
    fn start_shelf(&mut self, context: &mut Context) {
        self.start_read(
            context,
            RequestKind::Shelf,
            format!("{API}/shelf"),
            96 * 1024,
        );
    }

    /// Borrows the chosen library's copy, or joins its queue.
    ///
    /// One tap either way. Choosing which library is already the decision, and
    /// a hold is no more reversible than a borrow -- both are undone at the
    /// library, not here -- so putting a confirmation in front of only one of
    /// them would teach the reader that the other is somehow less of a choice.
    fn start_take(&mut self, context: &mut Context, hold: bool) {
        if !self.clear_for_request(context) {
            self.note = Some("Still finishing the last request. Try again in a moment.".to_owned());
            self.show(context);
            return;
        }
        let Some(result) = self.detail.as_ref() else {
            return;
        };
        let Some(source) = result.sources.get(self.selected_source) else {
            return;
        };
        let (kind, url, request) = if hold {
            (RequestKind::Hold, format!("{API}/holds"), "hold")
        } else {
            (RequestKind::Borrow, format!("{API}/jobs"), "borrow")
        };
        let body = ObjectBuilder::new()
            .set("publication_handle", source.handle.clone())
            .set(
                "client_request_id",
                format!("kobo-{request}-{}", unique_request_suffix()),
            )
            .build()
            .to_json();
        let task = Task::Post {
            url,
            body,
            content_type: "application/json".to_owned(),
            credential: Some(Self::credential()),
            headers: Vec::new(),
            max_bytes: 48 * 1024,
        };
        // The reader stays on the book they were reading about; the progress is
        // drawn under its title rather than on a list they did not ask for, and
        // it is on screen before this returns.
        self.note = None;
        self.watch = Some(Watch {
            view: View::Detail,
            job: None,
        });
        if let Some(task_id) = context.spawn(task) {
            self.inflight = Some((task_id, kind));
        } else {
            self.watch = None;
            self.note = Some(if hold {
                "The Kobo runtime is busy. The hold was not placed.".to_owned()
            } else {
                "The Kobo runtime is busy. The borrow was not sent.".to_owned()
            });
        }
        self.show(context);
    }

    fn start_return(&mut self, context: &mut Context) {
        if !self.clear_for_request(context) {
            return;
        }
        let Some(index) = self.selected_book else {
            return;
        };
        let books = self.filtered_books();
        let Some(book) = books.get(index) else {
            return;
        };
        let body = ObjectBuilder::new()
            .set(
                "client_request_id",
                format!("kobo-return-{}", unique_request_suffix()),
            )
            .build()
            .to_json();
        let task = Task::Post {
            url: format!("{API}/books/{}/return", book.id),
            body,
            content_type: "application/json".to_owned(),
            credential: Some(Self::credential()),
            headers: Vec::new(),
            max_bytes: 48 * 1024,
        };
        self.view = View::Library;
        self.note = None;
        self.watch = Some(Watch {
            view: View::Library,
            job: None,
        });
        if let Some(task_id) = context.spawn(task) {
            self.inflight = Some((task_id, RequestKind::Return));
        } else {
            self.watch = None;
            self.note = Some("The Kobo runtime is busy. The return was not sent.".to_owned());
        }
        self.show(context);
    }

    fn start_hook_retry(&mut self, context: &mut Context) {
        if !self.clear_for_request(context) {
            return;
        }
        let Some(job) = self.resolve_target() else {
            return;
        };
        if job.state != "hook_failed" {
            return;
        }
        let task = Task::Post {
            url: format!("{API}/jobs/{}/retry-hook", job.id),
            body: String::new(),
            content_type: "application/json".to_owned(),
            credential: Some(Self::credential()),
            headers: Vec::new(),
            max_bytes: 48 * 1024,
        };
        self.note = None;
        if let Some(task_id) = context.spawn(task) {
            self.inflight = Some((task_id, RequestKind::RetryHook));
        } else {
            self.note = Some("The Kobo runtime is busy. Nothing was sent.".to_owned());
        }
        self.show(context);
    }

    /// Records that the reader looked at their library account.
    ///
    /// It changes server state, so it goes through the same non-retrying path
    /// as a borrow or a return even though the server makes it idempotent: a
    /// decision a person made once should not be replayed by the runtime behind
    /// their back, and the button is still there if the network dropped.
    fn start_acknowledge(&mut self, context: &mut Context) {
        if !self.clear_for_request(context) {
            return;
        }
        let Some(job) = self.resolve_target() else {
            return;
        };
        let task = Task::Post {
            url: format!("{API}/jobs/{}/acknowledge", job.id),
            body: String::new(),
            content_type: "application/json".to_owned(),
            credential: Some(Self::credential()),
            headers: Vec::new(),
            max_bytes: 48 * 1024,
        };
        self.note = None;
        if let Some(task_id) = context.spawn(task) {
            self.inflight = Some((task_id, RequestKind::Acknowledge));
        } else {
            self.note = Some("The Kobo runtime is busy. Nothing was cleared.".to_owned());
        }
        self.show(context);
    }

    /// Asks for another tick only while something is actually running on the
    /// screen the reader is looking at.
    fn schedule_poll(&mut self, context: &mut Context) {
        if self.sleep_task.is_some() {
            return;
        }
        // A watch with no answer yet is left to the request it is waiting on;
        // only a job the server said is still running earns another tick.
        let running = self.watch.as_ref().is_some_and(|watch| {
            watch.view == self.view
                && watch
                    .job
                    .as_ref()
                    .is_some_and(|job| is_active_state(&job.state))
        });
        if running {
            self.sleep_task = context.spawn(Task::Sleep {
                seconds: POLL_SECONDS,
            });
        }
    }

    fn stop_poll(&mut self, context: &mut Context) {
        if let Some(task) = self.sleep_task.take() {
            context.cancel(task);
        }
    }

    /// Drops the watch when the reader goes somewhere it is not drawn.
    ///
    /// The request keeps running on the server; what stops is this app asking
    /// about it. A borrow that finishes after they walked away is in Library,
    /// and one that failed is in the section of Library for that.
    fn leave_watch(&mut self, context: &mut Context, destination: View) {
        if self
            .watch
            .as_ref()
            .is_some_and(|watch| watch.view != destination)
        {
            self.watch = None;
            self.stop_poll(context);
        }
    }

    fn poll_watch(&mut self, context: &mut Context) {
        if self.inflight.is_some() {
            self.schedule_poll(context);
            return;
        }
        let Some(watch) = &self.watch else {
            return;
        };
        if watch.view != self.view {
            return;
        }
        let Some(job) = &watch.job else {
            return;
        };
        if !is_active_state(&job.state) {
            return;
        }
        let task = Task::Fetch {
            url: format!("{API}/jobs/{}", job.id),
            offset: 0,
            max_bytes: 48 * 1024,
            credential: Some(Self::credential()),
            headers: vec![Header::new("Accept", "application/json")],
        };
        if let Some(task_id) = context.spawn_retrying(task) {
            self.inflight = Some((task_id, RequestKind::Job));
        }
    }

    fn drain_queued(&mut self, context: &mut Context) -> bool {
        if self.inflight.is_some() {
            return false;
        }
        match self.queued_request.take() {
            Some(RequestKind::Books) => {
                self.start_books(context);
                true
            }
            Some(RequestKind::Jobs) => {
                self.start_jobs(context);
                true
            }
            Some(RequestKind::Shelf) => {
                self.start_shelf(context);
                true
            }
            Some(RequestKind::Discovery) => {
                self.start_discovery(context);
                true
            }
            Some(RequestKind::CategoryList) => {
                self.start_categories(context);
                true
            }
            Some(RequestKind::BookstoreCategories) => {
                self.start_bookstore_categories(context);
                true
            }
            Some(RequestKind::Browse) => {
                self.start_browse(context);
                true
            }
            Some(RequestKind::Related) => {
                self.start_related(context);
                true
            }
            Some(RequestKind::Search) => {
                self.start_search(context);
                true
            }
            _ => false,
        }
    }

    #[allow(clippy::single_match_else, clippy::too_many_lines)]
    fn handle_completed(&mut self, context: &mut Context, kind: RequestKind, body: &[u8]) {
        match kind {
            RequestKind::Search => match parse_search(body) {
                Some(results) => {
                    self.results = results.into_iter().take(MAX_RESULTS).collect();
                    self.note = parse_catalog_status_note(body);
                }
                None => {
                    self.note =
                        Some("The proxy returned an unreadable search response.".to_owned());
                }
            },
            // Neither of these clears the note. They are also the reads that
            // follow an acknowledgement, and wiping the confirmation of a
            // decision the reader just made would leave nothing on screen to
            // say it landed. A note is cleared where one is asked for: a
            // refresh, or a move to another screen.
            RequestKind::Discovery => match parse_groups(body) {
                Some(groups) => {
                    self.groups = groups;
                    self.groups_page = 0;
                    if !self.favorites.is_empty() && self.bookstore_categories.is_empty() {
                        self.queued_request = Some(RequestKind::BookstoreCategories);
                    }
                }
                None => {
                    self.note = Some("The proxy returned an unreadable home page.".to_owned());
                }
            },
            RequestKind::CategoryList => match parse_categories(body) {
                Some(categories) => {
                    self.categories = categories;
                    self.categories_page = 0;
                }
                None => {
                    self.note = Some("The proxy returned an unreadable subject list.".to_owned());
                }
            },
            RequestKind::BookstoreCategories => match parse_bookstore_categories(body) {
                Some(categories) => {
                    self.bookstore_categories = categories;
                    self.bookstore_categories_page = 0;
                }
                None => {
                    self.note = Some(
                        "The proxy returned an unreadable bookstore category list.".to_owned(),
                    );
                }
            },
            RequestKind::Browse => match parse_browse(body) {
                Some((publications, sorts, paging)) => {
                    self.browsed = publications;
                    if !sorts.is_empty() {
                        self.sorts = sorts;
                    }
                    self.browse_paging = paging;
                    self.browse.page = paging.page.max(1);
                    self.browse_offset = if self.browse_from_end {
                        self.browsed
                            .len()
                            .div_ceil(self.browse_rows_per_page())
                            .saturating_sub(1)
                    } else {
                        0
                    };
                    self.browse_from_end = false;
                }
                None => {
                    self.browse_from_end = false;
                    self.note = Some("The proxy returned an unreadable book list.".to_owned());
                }
            },
            RequestKind::Related => match parse_publications(body) {
                Some(publications) => {
                    self.related = publications;
                    self.related_page = 0;
                }
                None => {
                    self.note = Some("The proxy returned an unreadable book list.".to_owned());
                }
            },
            RequestKind::Cover => {
                self.cover = decode_cover(body).and_then(|(width, height, grey)| {
                    context.put_picture(COVER_PICTURE_HANDLE, width, height, grey)
                });
                self.start_bookstore_category(context);
            }
            RequestKind::BookstoreCategory => {
                if let Some((state, path)) = parse_bookstore_category(body) {
                    self.bookstore_category_state = Some(state);
                    self.bookstore_category = path;
                } else {
                    self.bookstore_category_state = Some("unknown".to_owned());
                    self.bookstore_category = None;
                }
            }
            RequestKind::Shelf => match parse_shelf(body) {
                Some(entries) => {
                    self.shelf = entries;
                    self.holds_page = 0;
                }
                None => {
                    self.note = Some("The proxy returned an unreadable list of loans.".to_owned());
                }
            },
            RequestKind::Books => match parse_books(body) {
                Some(books) => {
                    self.books = books;
                    self.books_page = 0;
                    // The loans and the things needing a person are one screen,
                    // so they are always read as a pair.
                    self.queued_request = Some(RequestKind::Jobs);
                }
                None => {
                    self.note =
                        Some("The proxy returned an unreadable library response.".to_owned());
                }
            },
            RequestKind::Jobs => match parse_jobs(body) {
                Some(jobs) => {
                    self.jobs = jobs;
                    // The dates and the holds come from the libraries rather
                    // than the home server, so Library is three reads.
                    self.queued_request = Some(RequestKind::Shelf);
                }
                None => {
                    self.note = Some("The proxy returned an unreadable request list.".to_owned());
                }
            },
            RequestKind::Job => match parse_job(body) {
                Some(job) => {
                    let settled = !is_active_state(&job.state);
                    if let Some(watch) = self.watch.as_mut() {
                        if watch
                            .job
                            .as_ref()
                            .is_some_and(|current| current.id == job.id)
                        {
                            watch.job = Some(job);
                        }
                    }
                    if settled {
                        self.queued_request = Some(RequestKind::Books);
                    } else {
                        self.schedule_poll(context);
                    }
                }
                None => {
                    self.note =
                        Some("The proxy returned an unreadable request response.".to_owned());
                }
            },
            RequestKind::Health => match parse_health(body) {
                Some((summary, signed_out)) => {
                    self.health = Some(summary);
                    self.health_advice = sign_in_advice(&signed_out);
                    self.note = None;
                }
                None => {
                    self.note =
                        Some("The proxy returned an unreadable health response.".to_owned());
                }
            },
            RequestKind::Borrow | RequestKind::Hold | RequestKind::Return => {
                match parse_job(body) {
                    Some(job) => {
                        self.note = None;
                        let settled = !is_active_state(&job.state);
                        // A reader who left before the server answered is not
                        // dragged back to a screen they walked away from.
                        if let Some(watch) = self.watch.as_mut() {
                            watch.job = Some(job);
                        }
                        if settled {
                            self.queued_request = Some(RequestKind::Books);
                        } else {
                            self.schedule_poll(context);
                        }
                    }
                    None => {
                        self.watch = None;
                        self.note =
                            Some("The proxy returned an unreadable job response.".to_owned());
                    }
                }
            }
            RequestKind::RetryHook => match parse_job(body) {
                Some(job) => {
                    self.note = None;
                    self.resolve_job = None;
                    self.trail.clear();
                    self.view = View::Library;
                    self.jobs.retain(|existing| existing.id != job.id);
                    let settled = !is_active_state(&job.state);
                    self.watch = Some(Watch {
                        view: View::Library,
                        job: Some(job),
                    });
                    if settled {
                        self.queued_request = Some(RequestKind::Books);
                    } else {
                        self.schedule_poll(context);
                    }
                }
                None => {
                    self.note = Some("The proxy returned an unreadable job response.".to_owned());
                }
            },
            RequestKind::Acknowledge => match parse_job(body) {
                Some(job) => {
                    self.jobs.retain(|existing| existing.id != job.id);
                    self.resolve_job = None;
                    self.trail.clear();
                    self.view = View::Library;
                    self.note =
                        Some("Cleared. You can try this again whenever you like.".to_owned());
                    self.queued_request = Some(RequestKind::Books);
                }
                None => {
                    self.note = Some("The proxy returned an unreadable job response.".to_owned());
                }
            },
        }
    }

    fn handle_failed(&mut self, context: &mut Context, kind: RequestKind, error: TaskError) {
        if kind == RequestKind::Cover {
            self.cover = None;
            self.start_bookstore_category(context);
            return;
        }
        let advice = Failure::of(error).advice;
        if matches!(
            kind,
            RequestKind::Borrow | RequestKind::Hold | RequestKind::Return | RequestKind::RetryHook
        ) {
            self.watch = None;
        }
        // A 409 and a 404 reach an application as the same refusal, so a
        // rejected borrow or return points at the one screen that can tell the
        // reader which it was and offer the way out.
        if matches!(
            kind,
            RequestKind::Borrow | RequestKind::Hold | RequestKind::Return
        ) && error == TaskError::NotFound
        {
            self.queued_request = Some(RequestKind::Books);
        }
        self.note = Some(match kind {
            RequestKind::Hold => match error {
                TaskError::Offline => {
                    "This Kobo is not on a network, so no hold was placed. Join Wi-Fi and try again."
                        .to_owned()
                }
                TaskError::NotFound => {
                    "The home server would not take this hold. Look in Library for anything that needs you."
                        .to_owned()
                }
                _ => format!("No hold was placed. {advice}"),
            },
            RequestKind::Borrow => match error {
                TaskError::Offline => {
                    "This Kobo is not on a network, so nothing was borrowed. Join Wi-Fi and try again."
                        .to_owned()
                }
                TaskError::NotFound => {
                    "The home server would not take this borrow. Look in Library for anything that needs you."
                        .to_owned()
                }
                _ => format!("Nothing was borrowed. {advice}"),
            },
            RequestKind::Return => match error {
                TaskError::Offline => {
                    "This Kobo is not on a network, so nothing was returned. Join Wi-Fi and try again."
                        .to_owned()
                }
                TaskError::NotFound => {
                    "The home server would not take this return. Look in Library for anything that needs you."
                        .to_owned()
                }
                _ => format!("Nothing was returned. {advice}"),
            },
            RequestKind::RetryHook => match error {
                TaskError::Offline => {
                    "This Kobo is not on a network, so nothing was sent. Join Wi-Fi and try again."
                        .to_owned()
                }
                _ => format!("Nothing was sent. {advice}"),
            },
            RequestKind::Acknowledge => match error {
                TaskError::Offline => {
                    "This Kobo is not on a network, so nothing was cleared. Join Wi-Fi and try again."
                        .to_owned()
                }
                _ => format!("Nothing was cleared. {advice}"),
            },
            _ => advice.to_owned(),
        });
    }

    /// Where the reader is standing, so that they can be sent back to it.
    fn here(&self) -> Place {
        Place {
            view: self.view,
            detail: self.detail.clone(),
            browse: self.browse.clone(),
        }
    }

    /// Goes one screen deeper, remembering the way back.
    fn deeper(&mut self, context: &mut Context, destination: View) {
        let here = self.here();
        self.leave_watch(context, destination);
        // Coming back to the same screen is not going anywhere, so the trail
        // does not grow: otherwise Back would need as many taps as the reader
        // has changed pages.
        if here.view != destination {
            self.trail.push(here);
        }
        self.view = destination;
        self.note = None;
    }

    fn back(&mut self, context: &mut Context) {
        let place = self.trail.pop().unwrap_or(Place {
            view: View::Discover,
            detail: None,
            browse: BrowseQuery::default(),
        });
        self.leave_watch(context, place.view);
        self.view = place.view;
        self.detail = place.detail;
        self.browse = place.browse;
        if self.view != View::Detail {
            self.cover = None;
            self.bookstore_category = None;
            self.bookstore_category_state = None;
            self.bookstore_category_handle = None;
        }
        self.note = None;
        // A screen that only ever exists as an answer to a request has nothing
        // to draw when it is reached again, so it asks again.
        match self.view {
            View::Discover if self.groups.is_empty() => self.start_discovery(context),
            View::Categories if self.categories.is_empty() => self.start_categories(context),
            View::BookstoreCategories if self.bookstore_categories.is_empty() => {
                self.start_bookstore_categories(context)
            }
            View::Browse if self.browsed.is_empty() => self.start_browse(context),
            _ => self.show(context),
        }
    }

    /// Moves to one of the places in the bar, which is never a step deeper.
    fn go(&mut self, context: &mut Context, destination: View) {
        self.leave_watch(context, destination);
        self.trail.clear();
        self.view = destination;
        self.note = None;
        match destination {
            View::Discover => {
                if self.groups.is_empty() {
                    self.start_discovery(context);
                } else {
                    self.show(context);
                }
            }
            View::Library => self.start_books(context),
            View::Settings => self.start_health(context),
            _ => self.show(context),
        }
    }

    /// Opens a list of books: a discovery group, a subject, or an author.
    fn open_browse(&mut self, context: &mut Context, query: BrowseQuery) {
        self.deeper(context, View::Browse);
        self.browse = query;
        self.browsed.clear();
        self.sorts.clear();
        self.browse_paging = Paging::default();
        self.browse_offset = 0;
        self.browse_number = 1;
        self.browse_from_end = false;
        self.filters_page = 0;
        self.start_browse(context);
    }

    fn apply_preset(&mut self, context: &mut Context, index: usize) {
        let Some((_, title, category, audience, available_only, sort)) =
            Self::browse_presets().get(index).copied()
        else {
            return;
        };
        if self.view == View::Filters
            && self
                .trail
                .last()
                .is_some_and(|place| place.view == View::Browse)
        {
            self.trail.pop();
        }
        self.view = View::Browse;
        self.browse = BrowseQuery {
            title: title.to_owned(),
            category: category.map(str::to_owned),
            audience: audience.map(str::to_owned),
            author: None,
            sort: sort.map(str::to_owned),
            page: 1,
            available_only,
            bookstore: false,
        };
        self.show_pdf = false;
        self.browsed.clear();
        self.sorts.clear();
        self.browse_paging = Paging::default();
        self.browse_offset = 0;
        self.browse_number = 1;
        self.start_browse(context);
    }

    fn open_detail(&mut self, context: &mut Context, publication: Publication) {
        self.deeper(context, View::Detail);
        self.cover = None;
        self.bookstore_category = None;
        self.bookstore_category_state = None;
        self.bookstore_category_handle = None;
        self.detail = Some(publication);
        self.selected_source = 0;
        self.start_cover(context);
        self.show(context);
    }

    /// Covers are fetched only after a reader opens a book. The proxy has
    /// already reduced the image to the Kobo's sixteen greys, so this request
    /// transfers pixels the panel can draw directly rather than making the
    /// device decode a publisher image on its own watchdog.
    fn start_cover(&mut self, context: &mut Context) {
        if !self.clear_for_request(context) {
            return;
        }
        let Some(handle) = self
            .detail
            .as_ref()
            .and_then(Publication::related_handle)
            .map(percent_encode)
        else {
            self.start_bookstore_category(context);
            return;
        };
        let task = Task::Fetch {
            url: format!(
                "{API}/publications/{handle}/cover?width={COVER_WIDTH}&height={COVER_HEIGHT}"
            ),
            offset: 0,
            max_bytes: COVER_MAX_BYTES,
            credential: Some(Self::credential()),
            headers: vec![Header::new("Accept", "application/octet-stream")],
        };
        if let Some(task_id) = context.spawn(task) {
            self.inflight = Some((task_id, RequestKind::Cover));
        } else {
            self.start_bookstore_category(context);
        }
    }

    fn start_bookstore_category(&mut self, context: &mut Context) {
        if !self.clear_for_request(context) {
            return;
        }
        let Some(handle) = self
            .detail
            .as_ref()
            .and_then(Publication::related_handle)
            .map(percent_encode)
        else {
            self.bookstore_category_state = Some("unknown".to_owned());
            return;
        };
        self.bookstore_category_handle = Some(handle.clone());
        self.bookstore_category_state = Some("unresolved".to_owned());
        let task = Task::Fetch {
            url: format!("{API}/publications/{handle}/category"),
            offset: 0,
            max_bytes: 16 * 1024,
            credential: Some(Self::credential()),
            headers: vec![Header::new("Accept", "application/json")],
        };
        if let Some(task_id) = context.spawn_retrying(task) {
            self.inflight = Some((task_id, RequestKind::BookstoreCategory));
        } else {
            self.bookstore_category_state = Some("unknown".to_owned());
        }
    }

    /// Turns a page, or asks the server for the next one.
    fn turn_page(&mut self, context: &mut Context, forward: bool) {
        match self.view {
            View::Results => {
                self.results_page = turned(
                    self.results_page,
                    forward,
                    self.results.len(),
                    self.results_rows_per_page(),
                );
            }
            View::Related => {
                self.related_page = turned(
                    self.related_page,
                    forward,
                    self.related.len(),
                    self.related_rows_per_page(),
                );
            }
            View::Filters => {
                let groups = self.filter_groups();
                let per_page = self.filter_groups_per_page(&groups);
                self.filters_page = turned(self.filters_page, forward, groups.len(), per_page);
            }
            View::Categories => {
                self.categories_page = turned(
                    self.categories_page,
                    forward,
                    self.categories.len(),
                    self.page_rows(),
                );
            }
            View::BookstoreCategories => {
                self.bookstore_categories_page = turned(
                    self.bookstore_categories_page,
                    forward,
                    self.bookstore_categories.len(),
                    self.bookstore_categories_rows_per_page(),
                );
            }
            View::Library => {
                let count = self.filtered_books().len();
                let per_page = self.library_rows_per_page();
                self.books_page = turned(self.books_page, forward, count, per_page);
            }
            View::Holds => {
                let count = self.filtered_holds().len();
                let per_page = self.holds_rows_per_page();
                self.holds_page = turned(self.holds_page, forward, count, per_page);
            }
            View::Discover => {
                self.groups_page = turned(
                    self.groups_page,
                    forward,
                    self.groups.len(),
                    DISCOVER_GROUPS_PER_PAGE,
                );
            }
            // One page of prose at a time, and the pages are worked out from the
            // panel rather than held: the blurb is already in hand, so there is
            // nothing to remember that measuring it again does not answer.
            View::Description => {
                self.description_page = turned(
                    self.description_page,
                    forward,
                    self.description_pages().len(),
                    1,
                );
            }
            View::Browse => {
                self.turn_browse(context, forward);
                return;
            }
            _ => {}
        }
        self.show(context);
    }

    /// The browse pager, which spans two things: the slice of the server's page
    /// on the panel, and the server's pages themselves.
    fn turn_browse(&mut self, context: &mut Context, forward: bool) {
        let slices = self.browsed.len().div_ceil(self.browse_rows_per_page());
        if forward {
            if self.browse_offset + 1 < slices {
                self.browse_offset += 1;
                self.browse_number += 1;
                self.show(context);
                return;
            }
            if !self.browse_paging.has_next {
                return;
            }
            self.browse.page = self.browse.page.max(1) + 1;
            self.browse_number += 1;
        } else {
            if self.browse_offset > 0 {
                self.browse_offset -= 1;
                self.browse_number = self.browse_number.saturating_sub(1).max(1);
                self.show(context);
                return;
            }
            if !self.browse_paging.has_previous || self.browse.page <= 1 {
                return;
            }
            self.browse.page -= 1;
            self.browse_number = self.browse_number.saturating_sub(1).max(1);
            self.browse_from_end = true;
        }
        self.browsed.clear();
        self.start_browse(context);
    }
}

impl KoboApp for PretNumerique {
    fn on_start(&mut self, context: &mut Context) {
        self.metrics = context.metrics();
        context.store().load(STORE_STATE);
        self.start_discovery(context);
    }

    fn on_store(&mut self, context: &mut Context, result: StoreResult) {
        if let StoreResult::Loaded { key, value } = result {
            if key == STORE_STATE {
                self.load_state(value.as_deref());
                if !self.favorites.is_empty() && self.bookstore_categories.is_empty() {
                    if self.inflight.is_some() {
                        self.queued_request = Some(RequestKind::BookstoreCategories);
                    } else {
                        self.start_bookstore_categories(context);
                    }
                }
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    fn on_action(&mut self, context: &mut Context, action: kobo_sdk::ActionId) {
        if let Some(event) = self.entry.handle(action) {
            if let Typing::Submitted(value) = event {
                self.query = value;
                self.save_state(context);
                self.start_search(context);
            } else {
                self.show(context);
            }
            return;
        }
        if action == action_id(READER) {
            // Painted before leaving, not after: this is the last screen this
            // app gets to draw, and E Ink holds it at no power while the stock
            // reader starts.
            self.stop_poll(context);
            self.view = View::Leaving;
            self.show(context);
            context.exit();
            return;
        }
        if action == action_id(BACK) {
            self.back(context);
            return;
        }
        if action == action_id(DISCOVER) {
            self.go(context, View::Discover);
            return;
        }
        if action == action_id(SEARCH) {
            self.deeper(context, View::Search);
            self.show(context);
            return;
        }
        if action == action_id(CATEGORIES) {
            self.deeper(context, View::Categories);
            if self.categories.is_empty() {
                self.start_categories(context);
            } else {
                self.show(context);
            }
            return;
        }
        if action == action_id(BOOKSTORE_CATEGORIES) {
            self.deeper(context, View::BookstoreCategories);
            if self.bookstore_categories.is_empty() {
                self.start_bookstore_categories(context);
            } else {
                self.show(context);
            }
            return;
        }
        if action == action_id(FILTERS) {
            self.deeper(context, View::Filters);
            self.filters_page = 0;
            self.show(context);
            return;
        }
        if action == action_id(HOLDS) {
            self.deeper(context, View::Holds);
            self.holds_page = 0;
            self.show(context);
            return;
        }
        if action == action_id(RELATED) {
            self.deeper(context, View::Related);
            self.start_related(context);
            return;
        }
        if action == action_id(DESCRIPTION) {
            self.deeper(context, View::Description);
            self.description_page = 0;
            self.show(context);
            return;
        }
        if action == action_id(PREVIOUS_PAGE) {
            self.turn_page(context, false);
            return;
        }
        if action == action_id(NEXT_PAGE) {
            self.turn_page(context, true);
            return;
        }
        if action == action_id(LIBRARY) || action == action_id(OPEN_LIBRARY) {
            self.go(context, View::Library);
            return;
        }
        if action == action_id(SETTINGS) || action == action_id(OPEN_SETTINGS) {
            self.go(context, View::Settings);
            return;
        }
        if action == action_id(EDIT_QUERY) {
            self.entry.open_with(self.query.clone());
            self.show(context);
            return;
        }
        for (name, filter) in [
            (FILTER_ALL, CatalogFilter::All),
            (FILTER_MONTREAL, CatalogFilter::Montreal),
            (FILTER_BANQ, CatalogFilter::Banq),
        ] {
            if action == action_id(name) {
                self.filter = filter;
                // A narrower list is a shorter list, so the page the reader was
                // on may not exist any more.
                self.books_page = 0;
                self.holds_page = 0;
                self.show(context);
                return;
            }
        }
        if action == action_id(SHOW_PDF) {
            self.show_pdf = !self.show_pdf;
            self.note = None;
            // On a list already on screen the change is what is in the list, so
            // it is asked for again from its first page. The filters screen is
            // that list's own, so a change made there re-reads it too and the
            // reader is left on the filters to make another.
            if matches!(self.view, View::Browse | View::Filters) {
                self.browse.page = 1;
                self.browse_number = 1;
                self.browse_offset = 0;
                self.browsed.clear();
                self.start_browse(context);
            } else {
                self.show(context);
            }
            return;
        }
        if action == action_id(SUBMIT_SEARCH) || action == action_id(REFRESH) {
            self.note = None;
            match self.view {
                View::Search | View::Results => self.start_search(context),
                View::Discover => self.start_discovery(context),
                View::Categories => self.start_categories(context),
                View::BookstoreCategories => self.start_bookstore_categories(context),
                View::Browse => self.start_browse(context),
                View::Related => self.start_related(context),
                View::Library | View::Holds => {
                    // A finished request has had its say; one still running has
                    // not, so a refresh does not throw its progress away.
                    if self.watch.as_ref().is_some_and(|watch| !watch.is_running()) {
                        self.watch = None;
                    }
                    self.start_books(context);
                }
                View::Settings => self.start_health(context),
                _ => {}
            }
            return;
        }
        if action == action_id(CANCEL) {
            self.back(context);
            return;
        }
        if action == action_id(CONFIRM_RETURN) {
            self.start_return(context);
            return;
        }
        if action == action_id(RETRY_HOOK) {
            self.start_hook_retry(context);
            return;
        }
        if action == action_id(ACKNOWLEDGE) {
            self.start_acknowledge(context);
            return;
        }
        if let Some(index) = action_index(action, "resolve", self.jobs.len()) {
            self.resolve_job = self.jobs.get(index).map(|job| job.id.clone());
            self.deeper(context, View::Resolve);
            self.show(context);
            return;
        }
        let opened = [
            ("result", self.results.len()),
            ("browse", self.browsed.len()),
            ("related", self.related.len()),
        ]
        .into_iter()
        .find_map(|(prefix, count)| {
            let index = action_index(action, prefix, count)?;
            match prefix {
                "result" => self.results.get(index),
                "browse" => self.browsed.get(index),
                _ => self.related.get(index),
            }
            .cloned()
        });
        if let Some(publication) = opened {
            self.open_detail(context, publication);
            return;
        }
        // A group's own books, addressed by the group they are drawn under so
        // that turning the page does not change what a row means.
        for (group_index, group) in self.groups.iter().enumerate() {
            if let Some(index) = action_index(
                action,
                &format!("group.{group_index}.book"),
                group.publications.len(),
            ) {
                let publication = group.publications[index].clone();
                self.open_detail(context, publication);
                return;
            }
        }
        if let Some(index) = action_index(action, "group", self.groups.len()) {
            let group = &self.groups[index];
            if let Some(category) = group.category.clone() {
                let query = BrowseQuery {
                    title: group.title.clone(),
                    category: Some(category),
                    audience: None,
                    author: None,
                    sort: None,
                    page: 1,
                    available_only: false,
                    bookstore: false,
                };
                self.open_browse(context, query);
            }
            return;
        }
        if let Some(index) = action_index(action, "category", self.categories.len()) {
            let category = &self.categories[index];
            let query = BrowseQuery {
                title: category.name.clone(),
                category: Some(category.key.clone()),
                audience: None,
                author: None,
                sort: None,
                page: 1,
                available_only: false,
                bookstore: false,
            };
            self.open_browse(context, query);
            return;
        }
        if let Some(index) =
            action_index(action, BOOKSTORE_CATEGORY, self.bookstore_categories.len())
        {
            let category = self.bookstore_categories[index].clone();
            self.open_browse(
                context,
                BrowseQuery {
                    title: category.path,
                    category: Some(category.id),
                    audience: None,
                    author: None,
                    sort: None,
                    page: 1,
                    available_only: false,
                    bookstore: true,
                },
            );
            return;
        }
        if let Some(index) =
            action_index(action, FAVORITE_CATEGORY, self.bookstore_categories.len())
        {
            self.toggle_favorite(context, index);
            return;
        }
        if let Some(index) = action_index(action, FAVORITE_BROWSE, self.favorites.len()) {
            let favorites = self.favorite_categories();
            if let Some(category) = favorites.get(index) {
                self.open_browse(
                    context,
                    BrowseQuery {
                        title: category.path.clone(),
                        category: Some(category.id.clone()),
                        audience: None,
                        author: None,
                        sort: None,
                        page: 1,
                        available_only: false,
                        bookstore: true,
                    },
                );
            }
            return;
        }
        if let Some(index) = self
            .detail
            .as_ref()
            .and_then(|book| action_index(action, "author", book.authors.len().min(3)))
        {
            let author = self.detail.as_ref().map(|book| book.authors[index].clone());
            if let Some(author) = author {
                let query = BrowseQuery {
                    title: format!("Books by {author}"),
                    category: None,
                    audience: None,
                    author: Some(author),
                    sort: None,
                    page: 1,
                    available_only: false,
                    bookstore: false,
                };
                self.open_browse(context, query);
            }
            return;
        }
        if let Some(index) = action_index(action, "sort", self.sorts.len()) {
            let sort = self.sorts[index].key.clone();
            if self.browse.sort.as_deref() != Some(sort.as_str()) {
                self.browse.sort = Some(sort);
                self.browse.page = 1;
                self.browse_number = 1;
                self.browse_offset = 0;
                self.browsed.clear();
                self.start_browse(context);
            }
            return;
        }
        if let Some(index) = action_index(action, PRESET, Self::browse_presets().len()) {
            self.apply_preset(context, index);
            return;
        }
        if let Some(index) = self
            .detail
            .as_ref()
            .and_then(|book| action_index(action, "source", book.sources.len()))
        {
            self.selected_source = index;
            let Some(book) = self.detail.as_ref() else {
                return;
            };
            let borrowable = book.is_available();
            let free = book.sources[index].available;
            if free {
                self.start_take(context, false);
            } else if borrowable {
                // Another library has it, so this row is not the offer.
                self.note = Some("That copy is out. Another library has one.".to_owned());
                self.show(context);
            } else {
                self.start_take(context, true);
            }
            return;
        }
        if action_index(action, "hold", self.filtered_holds().len()).is_some() {
            self.note = Some("A hold can only be cancelled from your library account.".to_owned());
            self.show(context);
            return;
        }
        let books = self.filtered_books();
        if let Some(index) = action_index(action, "book", books.len()) {
            self.open_book(context, index, &books);
        }
    }

    fn on_task(&mut self, context: &mut Context, task: TaskId, outcome: TaskOutcome) {
        if self.sleep_task == Some(task) {
            self.sleep_task = None;
            self.poll_watch(context);
            self.show(context);
            return;
        }
        let Some((waiting, kind)) = self.inflight else {
            return;
        };
        if waiting != task {
            return;
        }
        self.inflight = None;
        match outcome {
            TaskOutcome::Completed(body) => self.handle_completed(context, kind, &body),
            TaskOutcome::Failed(error) => self.handle_failed(context, kind, error),
            TaskOutcome::Cancelled => {
                self.note = Some("The request was cancelled.".to_owned());
            }
        }
        if !self.drain_queued(context) {
            self.show(context);
        }
    }

    /// Nothing drawn from here is seen, so nothing is asked for either.
    fn on_background(&mut self, context: &mut Context) {
        self.stop_poll(context);
    }

    fn on_foreground(&mut self, context: &mut Context) {
        if self.view == View::Leaving {
            // Only reachable when this app was opened from the launcher rather
            // than presented as home: the exit went back there, and returning
            // must not land on a screen about having left.
            self.view = View::Discover;
        }
        if self.view == View::Library {
            self.start_books(context);
        }
        // Also re-arms the tick that `on_background` called off, whether or not
        // the loans are being read right now.
        self.poll_watch(context);
        self.show(context);
    }
}

impl PretNumerique {
    /// Routes a tap on a loan to whichever of its three meanings applies.
    fn open_book(&mut self, context: &mut Context, index: usize, books: &[Book]) {
        let Some(book) = books.get(index) else {
            return;
        };
        self.note = None;
        if let Some(job) = self.book_issue(book) {
            self.resolve_job = Some(job.id.clone());
            self.deeper(context, View::Resolve);
            self.show(context);
            return;
        }
        if book.return_state.as_deref().is_some_and(is_active_state) {
            self.note =
                Some("That return is still going. It finishes on the home server.".to_owned());
            self.show(context);
            return;
        }
        self.selected_book = Some(index);
        self.deeper(context, View::ConfirmReturn);
        self.show(context);
    }
}

fn action_index(action: kobo_sdk::ActionId, prefix: &str, count: usize) -> Option<usize> {
    (0..count).find(|index| action == action_id(&format!("{prefix}.{index}")))
}

/// Everyone who wrote it, for the one screen with room to name them all.
fn author_line(authors: &[String]) -> String {
    if authors.is_empty() {
        "Unknown author".to_owned()
    } else {
        authors.join(", ")
    }
}

/// The first author, shortened, and how many others there are.
///
/// A row has one line for everything under the title, and five Québécois names
/// set end to end is two of them: "Jocelyn Boisvert, Julie Champagne, Denis
/// Côté, Alexandre Côté-Fournier, Pierrette Dubé" cost a row of books to say
/// what "J. Boisvert +4" says. The whole list is on the book's own screen,
/// which is where a reader who wants it is going anyway.
fn short_author_line(authors: &[String]) -> String {
    match authors {
        [] => "Unknown author".to_owned(),
        [only] => shortened_name(only),
        [first, rest @ ..] => format!("{} +{}", shortened_name(first), rest.len()),
    }
}

/// A name as initials and a family name, where that is shorter than the name.
///
/// Written for the names these catalogues actually carry. A particle keeps its
/// own words -- "Jean de la Fontaine" is not three given names -- and is known
/// by its lower case, which is how French, Dutch and Spanish all write one. A
/// hyphenated given name keeps both halves, because "M. Tremblay" cannot tell
/// Marie-Ève from Marc-Antoine. A one-word name has nothing to shorten around,
/// and a name whose short form is no shorter is left alone: "Ed Yong" is not
/// improved by being called "E. Yong".
fn shortened_name(name: &str) -> String {
    let words = name.split_whitespace().collect::<Vec<_>>();
    if words.len() < 2 {
        return name.to_owned();
    }
    let family = words
        .iter()
        .position(|word| starts_lowercase(word))
        .unwrap_or(words.len() - 1)
        .min(words.len() - 1);
    if family == 0 {
        return name.to_owned();
    }
    let given = words[..family]
        .iter()
        .map(|word| initials_of(word))
        .collect::<Vec<_>>()
        .join(" ");
    let short = format!("{given} {}", words[family..].join(" "));
    if short.chars().count() < name.chars().count() {
        short
    } else {
        name.to_owned()
    }
}

/// "Marie-Ève" as "M.-È.". Counted in characters and upper-cased through
/// `char::to_uppercase`, because an accented capital is one character in this
/// catalogue's names and half of them carry one.
fn initials_of(word: &str) -> String {
    word.split('-')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut initial = part
                .chars()
                .next()
                .into_iter()
                .flat_map(char::to_uppercase)
                .collect::<String>();
            initial.push('.');
            initial
        })
        .collect::<Vec<_>>()
        .join("-")
}

fn starts_lowercase(word: &str) -> bool {
    word.chars().next().is_some_and(char::is_lowercase)
}

fn slice_of<T>(items: &[T], page: usize, per_page: usize) -> &[T] {
    let per_page = per_page.max(1);
    let start = (page * per_page).min(items.len());
    let end = (start + per_page).min(items.len());
    &items[start..end]
}

/// The page a turn lands on, which is never past either end.
fn turned(page: usize, forward: bool, len: usize, per_page: usize) -> usize {
    let last = len.div_ceil(per_page).saturating_sub(1);
    if forward {
        (page + 1).min(last)
    } else {
        page.saturating_sub(1)
    }
}

/// Names a few libraries the way a sentence would.
fn library_list(names: &[&str]) -> String {
    match names {
        [] => String::new(),
        [only] => (*only).to_owned(),
        [first, second] => format!("{first} and {second}"),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// What a reader needs to know about a book that two libraries both carry.
///
/// One library being out of copies is not the book being unavailable. So this
/// is the best answer across every library, and it names the one that has it,
/// because that is the library the reader is about to tap.
fn availability_summary(publication: &Publication) -> String {
    let available = publication.available_libraries();
    if available.is_empty() {
        let all = publication
            .sources
            .iter()
            .map(|source| source.catalog_name.as_str())
            .collect::<Vec<_>>();
        if all.is_empty() {
            return "No library has this".to_owned();
        }
        return format!("Every copy is out at {}", library_list(&all));
    }
    format!("Available now at {}", library_list(&available))
}

/// Whether a reader can have this now, in the fewest words that answer it.
///
/// A row does not say which library, and naming one was worse than useless
/// here: "Every copy is out at Montréal · Goodreads 3.0/5" is a wrapped line
/// under every title in the list, and the reader scanning that list has not yet
/// decided which book they want, let alone which library to take it from. The
/// book's own screen names every library and its state, and it is the only
/// screen where that can be acted on -- the library chooser is on it.
fn availability_state(publication: &Publication) -> &'static str {
    if !publication.available_libraries().is_empty() {
        "Available"
    } else if publication.sources.is_empty() {
        "Not available"
    } else {
        "All copies out"
    }
}

fn publication_summary(publication: &Publication) -> String {
    let goodreads = publication
        .goodreads_rating
        .map(|rating| format!(" · Goodreads {rating:.1}/5"))
        .unwrap_or_default();
    // A PDF on a six-inch panel is a page of a paper book photographed, and no
    // amount of type setting fixes it. The reader is told before they borrow.
    let format = if publication.pdf_only { " · PDF" } else { "" };
    format!(
        "{} · {}{format}{goodreads}",
        short_author_line(&publication.authors),
        availability_state(publication)
    )
}

fn category_summary(category: &Category) -> String {
    let libraries = category
        .libraries
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    match (category.total, libraries.is_empty()) {
        (Some(total), false) => format!(
            "{} books · {}",
            count_label(total),
            library_list(&libraries)
        ),
        (Some(total), true) => format!("{} books", count_label(total)),
        (None, false) => library_list(&libraries),
        (None, true) => "Both libraries".to_owned(),
    }
}

fn bookstore_category_summary(category: &BookstoreCategory) -> String {
    let held = format!("{} in your libraries", count_label(category.held));
    category.bookstore_total.map_or(held.clone(), |total| {
        format!("{held} · {} in leslibraires.ca", count_label(total))
    })
}

fn group_count(group: &Group) -> String {
    match group.total {
        Some(total) if total > 0 => format!("{} books in this list", count_label(total)),
        _ => "Everything in this list".to_owned(),
    }
}

fn holds_summary(holds: &[ShelfEntry]) -> String {
    let ready = holds
        .iter()
        .filter(|hold| hold.position.is_some_and(|position| position <= 1))
        .count();
    let waiting = match holds.len() {
        1 => "1 book".to_owned(),
        many => format!("{many} books"),
    };
    if ready > 0 {
        return format!("{waiting} · {ready} at the front of the queue");
    }
    waiting
}

/// One book being kept for a reader: where, how far up the queue, and for how
/// long.
///
/// Kept to one line. A row whose summary wraps is a row taller than the ones
/// around it, and a list of holds is a list of the same four facts each time.
fn hold_summary(hold: &ShelfEntry) -> String {
    let mut parts = vec![catalog_label(&hold.catalog).to_owned()];
    if let Some(position) = hold.position {
        parts.push(match hold.total {
            Some(total) if total > 0 => format!("{} of {total}", ordinal(position)),
            _ => format!("{} in line", ordinal(position)),
        });
    }
    if let Some(span) = plain_span(hold.since.as_deref(), hold.until.as_deref()) {
        parts.push(span);
    }
    parts.join(" · ")
}

/// Two dates as one span, dropping the year from the first when both share it.
fn plain_span(since: Option<&str>, until: Option<&str>) -> Option<String> {
    let since = since.and_then(plain_date);
    let until = until.and_then(plain_date);
    match (since, until) {
        (Some(since), Some(until)) => {
            let year = |date: &str| date.rsplit(' ').next().unwrap_or_default().to_owned();
            let opening = if year(&since) == year(&until) {
                since
                    .rsplit_once(' ')
                    .map_or(since.clone(), |(rest, _)| rest.to_owned())
            } else {
                since
            };
            Some(format!("{opening} to {until}"))
        }
        (Some(since), None) => Some(format!("since {since}")),
        (None, Some(until)) => Some(format!("until {until}")),
        (None, None) => None,
    }
}

fn ordinal(value: i64) -> String {
    let suffix = match (value % 10, value % 100) {
        (_, 11..=13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    };
    format!("{value}{suffix}")
}

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// An ISO date as a reader would read it: `2026-09-11T19:06:05Z` becomes
/// `11 September 2026`. Anything else is not a date and is not drawn.
fn plain_date(value: &str) -> Option<String> {
    let date = value.split('T').next()?;
    let mut parts = date.split('-');
    let year: u16 = parts.next()?.parse().ok()?;
    let month: usize = parts.next()?.parse().ok()?;
    let day: u8 = parts.next()?.parse().ok()?;
    if !(1..=12).contains(&month) || day == 0 || day > 31 {
        return None;
    }
    Some(format!("{day} {} {year}", MONTHS[month - 1]))
}

/// When an edition came out, as a person would say it: `April 2019`, or a bare
/// year where that is all the catalogue knows.
///
/// The day is dropped rather than drawn. Nobody asks which Tuesday a novel came
/// out, and half the libraries' dates are an evening timestamp in UTC -- the
/// previous day in this time zone -- so the day is the one part of the answer
/// that could be wrong. That leaves the month wrong only where such a date
/// falls on the last of one, and the server normalises them.
///
/// Anything that is not a date at all draws nothing: a returned `None` costs no
/// row, where a fallback would put the server's own field on the panel.
fn published_label(value: &str) -> Option<String> {
    let date = value.trim().split('T').next()?;
    let mut parts = date.split('-');
    let year: u16 = parts.next()?.trim().parse().ok()?;
    if !(1000..=3000).contains(&year) {
        return None;
    }
    match parts
        .next()
        .and_then(|month| month.trim().parse::<usize>().ok())
    {
        Some(month) if (1..=12).contains(&month) => Some(format!("{} {year}", MONTHS[month - 1])),
        _ => Some(year.to_string()),
    }
}

/// How much book there is: pages for a book, hours and minutes for a recording.
///
/// One fact rather than two, because a title has one or the other and almost
/// never both -- an audiobook has no page count and a book has no running time.
/// A book that somehow has both says both, in one line.
fn length_label(book: &Publication) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(pages) = book.number_of_pages {
        parts.push(format!("{} pages", count_label(pages)));
    }
    if let Some(seconds) = book.duration {
        parts.push(listening_time(seconds));
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// A recording's length, rounded to the minute: `9 h 12 min`, or `45 min` for
/// anything under the hour. Seconds are noise on a nine hour audiobook.
fn listening_time(seconds: i64) -> String {
    let minutes = (seconds + 30) / 60;
    let hours = minutes / 60;
    if hours == 0 {
        return format!("{minutes} min");
    }
    match minutes % 60 {
        0 => format!("{hours} h"),
        rest => format!("{hours} h {rest} min"),
    }
}

/// Whether two lists from two different systems are talking about one book.
///
/// The home server's identifier means nothing to the catalogue and the
/// catalogue's means nothing here, so the title is all they share.
fn same_title(one: &str, other: &str) -> bool {
    fn key(value: &str) -> String {
        value
            .chars()
            .filter(|character| character.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect()
    }
    !one.is_empty() && key(one) == key(other)
}

/// Escapes a value for a query string. The values are titles, subjects and
/// authors' names, so accents and spaces are the normal case.
fn percent_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(char::from(*byte));
            }
            other => {
                use std::fmt::Write as _;
                let _ = write!(encoded, "%{other:02X}");
            }
        }
    }
    encoded
}

fn compact_message(message: &str, limit: usize) -> String {
    let mut compact = message.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() > limit {
        let suffix = "...";
        if limit <= suffix.chars().count() {
            return suffix.chars().take(limit).collect();
        }
        compact = compact
            .chars()
            .take(limit.saturating_sub(suffix.chars().count()))
            .collect();
        compact.push_str(suffix);
    }
    compact
}

fn count_label(value: i64) -> String {
    let digits = value.max(0).to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, character) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(character);
    }
    grouped
}

fn catalog_label(catalog: &str) -> &'static str {
    match catalog {
        "montreal" => "Montréal",
        "banq" => "BAnQ",
        _ => "Library",
    }
}

fn kind_label(kind: &str) -> &str {
    match kind {
        "borrow" => "Borrow",
        "hold" => "Hold",
        "return" => "Return",
        "import" => "Send",
        _ => "Request",
    }
}

/// The row for something that has no loan of its own to sit against.
fn unresolved_summary(job: &Job) -> String {
    let prefix = format!(
        "{} · {}",
        catalog_label(&job.catalog),
        kind_label(&job.kind)
    );
    match job.state.as_str() {
        "borrow_uncertain" | "return_uncertain" | "hold_uncertain" => {
            format!("{prefix} · Check your library account")
        }
        "hook_failed" => format!("{prefix} · Not sent to your reader"),
        "failed" => format!("{prefix} · Did not finish"),
        other => format!("{prefix} · {}", state_label(other)),
    }
}

/// What a finished request says once, in place, where it was started.
fn settled_message(job: &Job) -> (BannerLevel, String) {
    let detail = job.error.as_deref().map_or_else(String::new, |error| {
        format!(" {}", compact_message(error, MAX_ERROR_CHARS))
    });
    let library = catalog_label(&job.catalog);
    match job.state.as_str() {
        // A resent delivery finishes in the same state as a borrow, and telling
        // the reader they borrowed a book they already had would be a lie.
        "complete" if job.kind == "import" => (
            BannerLevel::Info,
            "Sent to your reader.".to_owned(),
        ),
        // A hold gives no book, so it must not borrow the borrow's words.
        "complete" | "held" | "hold_placed" if job.kind == "hold" => (
            BannerLevel::Info,
            format!("You are in line at {library}. Your holds are in Library."),
        ),
        "complete" => (
            BannerLevel::Info,
            "Borrowed. It is in your Library now.".to_owned(),
        ),
        "held" | "hold_placed" => (
            BannerLevel::Info,
            format!("You are in line at {library}."),
        ),
        "hold_uncertain" => (
            BannerLevel::Attention,
            format!("The home server cannot tell whether {library} put you in line. Check your library account.{detail}"),
        ),
        "returned" => (
            BannerLevel::Info,
            format!("Returned. {library} has it back."),
        ),
        "auth_required" => (
            BannerLevel::Attention,
            format!("Nothing was borrowed: the home server has to sign in to {library} again.{detail}"),
        ),
        "hook_failed" => (
            BannerLevel::Attention,
            format!("Borrowed, but not sent to your reader.{detail}"),
        ),
        "borrow_uncertain" => (
            BannerLevel::Attention,
            format!("The home server cannot tell whether {library} gave you this loan. Check your library account.{detail}"),
        ),
        "failed" => (
            BannerLevel::Attention,
            format!("This did not finish, and nothing was borrowed.{detail}"),
        ),
        "return_failed" => (
            BannerLevel::Attention,
            format!("Return failed · loan kept.{detail}"),
        ),
        "return_uncertain" => (
            BannerLevel::Attention,
            format!("The home server cannot tell whether {library} took this loan back. Check your library account.{detail}"),
        ),
        other => (BannerLevel::Attention, state_label(other).to_owned()),
    }
}

/// The whole of what the reader is being asked to decide.
fn resolve_explanation(job: &Job) -> String {
    let library = catalog_label(&job.catalog);
    match job.state.as_str() {
        "hook_failed" => "The loan and the book file are both safe on your home server. Only the copy to your reader did not go through, and sending it again is safe.".to_owned(),
        "borrow_uncertain" => format!(
            "The home server cannot tell whether {library} gave you this loan.\n\nOpen your {library} account and look for it. Then tap below. That only clears this message: it borrows nothing and returns nothing."
        ),
        "hold_uncertain" => format!(
            "The home server cannot tell whether {library} put you in line for this book.\n\nOpen your {library} account and look. Then tap below. That only clears this message: it places no hold and cancels none."
        ),
        "return_uncertain" => format!(
            "The home server cannot tell whether {library} took this loan back.\n\nOpen your {library} account and look. Then tap below. That only clears this message: it returns nothing."
        ),
        "return_failed" => format!(
            "{library} did not take this loan back, so you still have it. Check your {library} account, then tap below to clear this message and try the return again."
        ),
        _ => format!(
            "This stopped before it finished. Check your {library} account in case part of it went through, then tap below to clear this message."
        ),
    }
}

fn availability_label(source: &Source) -> String {
    let value = source.availability.trim();
    if source.available {
        if value.is_empty() || value.eq_ignore_ascii_case("available") {
            "Available now".to_owned()
        } else {
            value.to_owned()
        }
    } else if value.is_empty()
        || value.eq_ignore_ascii_case("available")
        || value.eq_ignore_ascii_case("unavailable")
    {
        "Not currently available".to_owned()
    } else {
        value.to_owned()
    }
}

fn state_label(state: &str) -> &str {
    match state {
        "ready" => "Signed in",
        "auth_required" => "Sign-in needed",
        "queued" => "Waiting to start",
        "borrowing" => "Borrowing...",
        "downloading" => "Saving book...",
        "stored" => "Saved on server",
        "hook_running" => "Sending...",
        "complete" => "Ready on server",
        "hook_failed" => "Not sent to your reader",
        "borrow_uncertain" => "Check account before retrying",
        "holding" => "Placing the hold...",
        "held" | "hold_placed" => "In line at the library",
        "hold_uncertain" => "Hold not confirmed",
        "returning" => "Returning...",
        "returned" => "Returned",
        "return_failed" => "Return failed · loan kept",
        "return_uncertain" => "Return not confirmed",
        "failed" => "Could not finish",
        _ => state,
    }
}

/// Whether a request only asks the server a question.
///
/// A read can be dropped and asked again; anything else has told the server to
/// do something and cannot be taken back.
fn is_read(kind: RequestKind) -> bool {
    matches!(
        kind,
        RequestKind::Search
            | RequestKind::Discovery
            | RequestKind::CategoryList
            | RequestKind::BookstoreCategories
            | RequestKind::Browse
            | RequestKind::Related
            | RequestKind::Shelf
            | RequestKind::Books
            | RequestKind::Jobs
            | RequestKind::Job
            | RequestKind::Health
            | RequestKind::Cover
            | RequestKind::BookstoreCategory
    )
}

fn is_active_state(state: &str) -> bool {
    matches!(
        state,
        "queued"
            | "borrowing"
            | "downloading"
            | "stored"
            | "hook_running"
            | "returning"
            | "holding"
    )
}

/// Names the one thing that fixes a signed-out catalog, which is not on this
/// device.
fn sign_in_advice(catalogs: &[String]) -> Option<String> {
    if catalogs.is_empty() {
        return None;
    }
    Some(
        catalogs
            .iter()
            .map(|catalog| {
                format!(
                    "{} is signed out. On the home server, run PretNumeriqueProxy --auth {catalog}. It cannot be done from this Kobo.",
                    catalog_label(catalog)
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n"),
    )
}

fn unique_request_suffix() -> String {
    // The request ID is for deduplication only; it carries no catalog data.
    format!(
        "{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_millis())
    )
}

/// Whichever of the names a list of books arrives under.
///
/// Search answers `results`, the home page and a browse answer `publications`.
/// Reading all of them costs one lookup and means a screen does not go blank
/// over a word.
fn publication_array(value: &Value) -> Option<&[Value]> {
    for key in ["results", "publications", "books", "items"] {
        if let Some(array) = value.get(key).and_then(Value::as_array) {
            return Some(array);
        }
    }
    value.as_array()
}

/// One optional string, under either of the names it arrives as, and nothing at
/// all when it arrives blank.
fn text_field(value: &Value, name: &str, alternate: &str) -> Option<String> {
    value
        .get(name)
        .or_else(|| value.get(alternate))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

/// Every library's copy of one book.
fn parse_sources(result: &Value) -> Vec<Source> {
    result
        .get("sources")
        .and_then(Value::as_array)
        .map(|sources| {
            sources
                .iter()
                .filter_map(|source| {
                    Some(Source {
                        handle: source.get("handle")?.as_str()?.to_owned(),
                        catalog: source.get("catalog")?.as_str()?.to_owned(),
                        catalog_name: source
                            .get("catalog_name")
                            .or_else(|| source.get("catalogName"))
                            .and_then(Value::as_str)?
                            .to_owned(),
                        availability: source
                            .get("availability")
                            .and_then(Value::as_str)
                            .unwrap_or("Availability unknown")
                            .to_owned(),
                        available: source
                            .get("is_available")
                            .or_else(|| source.get("isAvailable"))
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn parse_publication(result: &Value) -> Option<Publication> {
    let title = result.get("title")?.as_str()?.to_owned();
    let authors = result
        .get("authors")
        .and_then(Value::as_array)
        .map(|authors| {
            authors
                .iter()
                // An author is a name, or an object that has one: the browse
                // the catalogue itself publishes is keyed on that name.
                .filter_map(|author| {
                    author
                        .as_str()
                        .or_else(|| author.get("name").and_then(Value::as_str))
                })
                .take(5)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let sources = parse_sources(result);
    let pdf_only = result
        .get("pdf_only")
        .or_else(|| result.get("isPdfOnly"))
        .and_then(Value::as_bool)
        .or_else(|| {
            result
                .get("format")
                .and_then(Value::as_str)
                .map(|format| format.eq_ignore_ascii_case("pdf"))
        })
        .or_else(|| {
            let formats = result.get("formats")?.as_array()?;
            let named = formats.iter().filter_map(Value::as_str).collect::<Vec<_>>();
            (!named.is_empty()).then(|| {
                named
                    .iter()
                    .all(|format| format.eq_ignore_ascii_case("pdf"))
            })
        })
        .unwrap_or(false);
    (!sources.is_empty()).then_some(Publication {
        pdf_only,
        handle: result
            .get("handle")
            .or_else(|| result.get("publication_handle"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        title,
        authors,
        isbn: result
            .get("isbn")
            .and_then(Value::as_str)
            .map(str::to_owned),
        description: result
            .get("description")
            .and_then(Value::as_str)
            .map(|value| compact_message(value, MAX_DESCRIPTION_CHARS)),
        // Read under both spellings, and dropped rather than kept when there is
        // nothing in it. A zero page count and an empty publisher are the same
        // thing as no field at all, and a screen that draws them says "Length:
        // 0 pages" about a book nobody counted.
        published: text_field(result, "published", "publishedAt"),
        number_of_pages: result
            .get("number_of_pages")
            .or_else(|| result.get("numberOfPages"))
            .and_then(Value::as_i64)
            .filter(|pages| *pages > 0),
        publisher: text_field(result, "publisher", "publisherName").or_else(|| {
            // The catalogue itself names a publisher as an object, the way it
            // names an author.
            result
                .get("publisher")
                .and_then(|publisher| publisher.get("name"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_owned)
        }),
        duration: result
            .get("duration")
            .and_then(Value::as_i64)
            .filter(|seconds| *seconds > 0),
        goodreads_rating: result.get("goodreads_rating").and_then(Value::as_f64),
        goodreads_ratings_count: result
            .get("goodreads_ratings_count")
            .and_then(Value::as_i64),
        goodreads_reviews_count: result
            .get("goodreads_reviews_count")
            .and_then(Value::as_i64),
        sources,
    })
}

fn parse_search(body: &[u8]) -> Option<Vec<Publication>> {
    parse_publications(body)
}

fn parse_publications(body: &[u8]) -> Option<Vec<Publication>> {
    let text = std::str::from_utf8(body).ok()?;
    let root = kobo_json::parse(text).ok()?;
    Some(
        publication_array(&root)?
            .iter()
            .filter_map(parse_publication)
            .collect(),
    )
}

fn parse_groups(body: &[u8]) -> Option<Vec<Group>> {
    let text = std::str::from_utf8(body).ok()?;
    let root = kobo_json::parse(text).ok()?;
    let groups = root.get("groups")?.as_array()?;
    Some(
        groups
            .iter()
            .filter_map(|group| {
                let publications = publication_array(group).map_or_else(Vec::new, |array| {
                    array.iter().filter_map(parse_publication).collect()
                });
                Some(Group {
                    title: group.get("title")?.as_str()?.to_owned(),
                    category: group
                        .get("category")
                        .or_else(|| group.get("key"))
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    total: group
                        .get("total")
                        .or_else(|| group.get("number_of_items"))
                        .and_then(Value::as_i64),
                    publications,
                })
            })
            .take(MAX_GROUPS)
            .collect(),
    )
}

fn parse_categories(body: &[u8]) -> Option<Vec<Category>> {
    let text = std::str::from_utf8(body).ok()?;
    let root = kobo_json::parse(text).ok()?;
    let categories = root
        .get("categories")
        .and_then(Value::as_array)
        .or_else(|| root.as_array())?;
    Some(
        categories
            .iter()
            .filter_map(|category| {
                let name = category.get("name")?.as_str()?.to_owned();
                let key = category
                    .get("key")
                    .or_else(|| category.get("category"))
                    .or_else(|| category.get("id"))
                    .and_then(Value::as_str)
                    .map_or_else(|| name.clone(), str::to_owned);
                Some(Category {
                    name,
                    key,
                    libraries: category
                        .get("catalog_names")
                        .or_else(|| category.get("catalogs"))
                        .and_then(Value::as_array)
                        .map(|names| {
                            names
                                .iter()
                                .filter_map(Value::as_str)
                                .map(|name| catalog_label(name).to_owned())
                                .collect()
                        })
                        .unwrap_or_default(),
                    total: category
                        .get("total")
                        .or_else(|| category.get("number_of_items"))
                        .and_then(Value::as_i64),
                })
            })
            .take(MAX_CATEGORIES)
            .collect(),
    )
}

fn parse_bookstore_categories(body: &[u8]) -> Option<Vec<BookstoreCategory>> {
    let text = std::str::from_utf8(body).ok()?;
    let root = kobo_json::parse(text).ok()?;
    Some(
        root.get("categories")?
            .as_array()?
            .iter()
            .filter_map(|category| {
                Some(BookstoreCategory {
                    id: category.get("id")?.as_str()?.to_owned(),
                    name: category.get("name")?.as_str()?.to_owned(),
                    path: category
                        .get("path")
                        .and_then(Value::as_str)
                        .unwrap_or_else(|| {
                            category.get("name").and_then(Value::as_str).unwrap_or("")
                        })
                        .to_owned(),
                    held: category.get("held")?.as_i64()?,
                    bookstore_total: category.get("bookstore_total").and_then(Value::as_i64),
                })
            })
            .take(MAX_CATEGORIES)
            .collect(),
    )
}

fn parse_bookstore_category(body: &[u8]) -> Option<(String, Option<String>)> {
    let text = std::str::from_utf8(body).ok()?;
    let root = kobo_json::parse(text).ok()?;
    let state = root.get("state")?.as_str()?.to_owned();
    let path = root
        .get("category")
        .and_then(|category| category.get("path"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    Some((state, path))
}

/// A browsed page, the orders it can be read in, and which way it can be
/// turned.
///
/// Whether there is another page is the server's answer, under whichever of
/// `has_next` and `next_page` it gives. Nothing here counts rows to decide it.
fn parse_browse(body: &[u8]) -> Option<(Vec<Publication>, Vec<Sort>, Paging)> {
    let text = std::str::from_utf8(body).ok()?;
    let root = kobo_json::parse(text).ok()?;
    let publications = publication_array(&root)?
        .iter()
        .filter_map(parse_publication)
        .collect();
    let sorts = root
        .get("sorts")
        .or_else(|| root.get("sort_options"))
        .and_then(Value::as_array)
        .map(|sorts| {
            sorts
                .iter()
                .filter_map(|sort| {
                    let key = sort
                        .get("key")
                        .or_else(|| sort.get("sort"))
                        .and_then(Value::as_str)?;
                    let label = sort
                        .get("label")
                        .or_else(|| sort.get("title"))
                        .and_then(Value::as_str)
                        .unwrap_or(key);
                    Some(Sort {
                        key: key.to_owned(),
                        label: label.to_owned(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let page = root
        .get("page")
        .or_else(|| root.get("current_page"))
        .and_then(Value::as_i64)
        .and_then(|page| u32::try_from(page).ok())
        .unwrap_or(1)
        .max(1);
    let flag = |names: [&str; 3]| {
        names.iter().find_map(|name| {
            root.get(name).and_then(|value| {
                value
                    .as_bool()
                    .or_else(|| value.as_i64().map(|number| number > 0))
            })
        })
    };
    Some((
        publications,
        sorts,
        Paging {
            page,
            total_pages: root
                .get("total_pages")
                .and_then(Value::as_i64)
                .and_then(|total| u32::try_from(total).ok()),
            has_previous: flag(["has_previous", "previous_page", "previous"]).unwrap_or(page > 1),
            has_next: flag(["has_next", "next_page", "next"]).unwrap_or(false),
        },
    ))
}

fn parse_shelf(body: &[u8]) -> Option<Vec<ShelfEntry>> {
    let text = std::str::from_utf8(body).ok()?;
    let root = kobo_json::parse(text).ok()?;
    let entries = ["entries", "items", "shelf"]
        .iter()
        .find_map(|key| root.get(key).and_then(Value::as_array))
        .or_else(|| root.as_array())?;
    Some(
        entries
            .iter()
            .filter_map(|entry| {
                let holds = entry.get("holds");
                Some(ShelfEntry {
                    title: entry.get("title")?.as_str()?.to_owned(),
                    authors: entry
                        .get("authors")
                        .and_then(Value::as_array)
                        .map(|authors| {
                            authors
                                .iter()
                                .filter_map(Value::as_str)
                                .take(3)
                                .map(str::to_owned)
                                .collect()
                        })
                        .unwrap_or_default(),
                    catalog: entry
                        .get("catalog")
                        .and_then(Value::as_str)
                        .unwrap_or("library")
                        .to_owned(),
                    kind: entry.get("kind")?.as_str()?.to_owned(),
                    since: entry
                        .get("since")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    until: entry
                        .get("until")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    position: entry
                        .get("position")
                        .or_else(|| holds.and_then(|holds| holds.get("position")))
                        .and_then(Value::as_i64),
                    total: entry
                        .get("total")
                        .or_else(|| holds.and_then(|holds| holds.get("total")))
                        .and_then(Value::as_i64),
                })
            })
            .take(MAX_SHELF)
            .collect(),
    )
}

fn parse_catalog_status_note(body: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(body).ok()?;
    let root = kobo_json::parse(text).ok()?;
    let statuses = root.get("catalogs")?.as_array()?;
    let messages = statuses
        .iter()
        .filter_map(|status| {
            let catalog = status.get("catalog").and_then(Value::as_str)?;
            let state = status.get("state").and_then(Value::as_str)?;
            match state {
                "auth_required" => Some(format!(
                    "{} needs authentication on the proxy.",
                    catalog_label(catalog)
                )),
                "error" => {
                    let detail = status
                        .get("message")
                        .and_then(Value::as_str)
                        .map(|message| compact_message(message, 120))
                        .filter(|message| !message.is_empty())
                        .unwrap_or_else(|| "try again later".to_owned());
                    Some(format!(
                        "{} could not be searched: {detail}",
                        catalog_label(catalog)
                    ))
                }
                _ => None,
            }
        })
        .collect::<Vec<_>>();
    (!messages.is_empty()).then(|| messages.join(" "))
}

/// Unpack the proxy's panel-ready cover body into the greys that
/// `Context::put_picture` accepts. The proxy owns image decoding, scaling and
/// dithering; the Kobo only validates a small bounded wire format and expands
/// each four-bit level back to the runtime's byte-per-pixel representation.
fn decode_cover(body: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    const HEADER_BYTES: usize = 8;
    const MAX_WIDTH: u32 = 1072;
    const MAX_HEIGHT: u32 = 1448;
    const MAGIC: &[u8; 4] = b"PNC1";

    if body.len() < HEADER_BYTES || &body[..4] != MAGIC {
        return None;
    }
    let width = u32::from(u16::from_be_bytes([body[4], body[5]]));
    let height = u32::from(u16::from_be_bytes([body[6], body[7]]));
    if width == 0 || height == 0 || width > MAX_WIDTH || height > MAX_HEIGHT {
        return None;
    }
    let pixels = usize::try_from(width)
        .ok()?
        .checked_mul(usize::try_from(height).ok()?)?;
    let packed_bytes = pixels.div_ceil(2);
    if body.len() != HEADER_BYTES + packed_bytes {
        return None;
    }

    let mut grey = Vec::with_capacity(pixels);
    for byte in &body[HEADER_BYTES..] {
        grey.push((byte >> 4) * 17);
        if grey.len() < pixels {
            grey.push((byte & 0x0f) * 17);
        }
    }
    Some((width, height, grey))
}

fn parse_books(body: &[u8]) -> Option<Vec<Book>> {
    let text = std::str::from_utf8(body).ok()?;
    let root = kobo_json::parse(text).ok()?;
    Some(
        root.as_array()?
            .iter()
            .filter_map(|book| {
                Some(Book {
                    id: book.get("id")?.as_str()?.to_owned(),
                    title: book.get("title")?.as_str()?.to_owned(),
                    catalog: book.get("catalog")?.as_str()?.to_owned(),
                    file_name: book
                        .get("fileName")
                        .or_else(|| book.get("file_name"))
                        .and_then(Value::as_str)
                        .unwrap_or("LCPL")
                        .to_owned(),
                    return_state: book
                        .get("returnState")
                        .or_else(|| book.get("return_state"))
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                })
            })
            .collect(),
    )
}

fn parse_job(body: &[u8]) -> Option<Job> {
    let text = std::str::from_utf8(body).ok()?;
    let value = kobo_json::parse(text).ok()?;
    parse_job_value(&value)
}

fn parse_job_value(value: &Value) -> Option<Job> {
    Some(Job {
        id: value.get("id")?.as_str()?.to_owned(),
        kind: value.get("kind")?.as_str()?.to_owned(),
        state: value.get("state")?.as_str()?.to_owned(),
        title: value.get("title")?.as_str().unwrap_or("Loan").to_owned(),
        catalog: value
            .get("catalog")
            .and_then(Value::as_str)
            .unwrap_or("library")
            .to_owned(),
        book_id: value
            .get("book_id")
            .or_else(|| value.get("bookId"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        error: value
            .get("error_message")
            .or_else(|| value.get("errorMessage"))
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

fn parse_jobs(body: &[u8]) -> Option<Vec<Job>> {
    let text = std::str::from_utf8(body).ok()?;
    let root = kobo_json::parse(text).ok()?;
    root.get("jobs")?.as_array().map(|jobs| {
        jobs.iter()
            .filter_map(parse_job_value)
            .take(MAX_JOBS)
            .collect()
    })
}

/// The catalogue summary, and the catalogues that are signed out.
fn parse_health(body: &[u8]) -> Option<(String, Vec<String>)> {
    let text = std::str::from_utf8(body).ok()?;
    let value = kobo_json::parse(text).ok()?;
    let catalogs = value.get("catalogs")?.as_array()?;
    let mut summary = Vec::new();
    let mut signed_out = Vec::new();
    for catalog in catalogs {
        let Some(name) = catalog.get("catalog").and_then(Value::as_str) else {
            continue;
        };
        let Some(state) = catalog.get("state").and_then(Value::as_str) else {
            continue;
        };
        summary.push(format!("{} {}", catalog_label(name), state_label(state)));
        if state != "ready" {
            signed_out.push(name.to_owned());
        }
    }
    Some((summary.join(" · "), signed_out))
}

fn main() -> ExitCode {
    match kobo_sdk::run("pret-numerique", PretNumerique::default()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("pret-numerique: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        availability_label, availability_summary, bookstore_category_summary, category_summary,
        compact_message, decode_cover, is_active_state, kind_label, ordinal, parse_books,
        parse_bookstore_categories, parse_bookstore_category, parse_browse,
        parse_catalog_status_note, parse_categories, parse_groups, parse_health, parse_job,
        parse_jobs, parse_publications, parse_search, parse_shelf, plain_date, publication_summary,
        published_label, resolve_explanation, settled_message, sign_in_advice, state_label, turned,
        unresolved_summary, Book, BookstoreCategory, BrowseQuery, Category, Group, Job, Paging,
        PretNumerique, Publication, ShelfEntry, Sort, Source, View, Watch, ACKNOWLEDGE, API, BACK,
        CONFIRM_RETURN, DESCRIPTION, FILTERS, NEXT_PAGE, PREVIOUS_PAGE, READER, RELATED,
        RETRY_HOOK, SEARCH, SHOW_PDF, SUBMIT_SEARCH,
    };
    use kobo_sdk::{
        action_id, AppRunner, BannerLevel, Command, Context, DiagnosticSeverity, KoboApp,
        StoreResult, Task, TaskError,
    };
    use kobo_ui::{
        Chrome, DisplayMetrics, LayoutIssueKind, LayoutKind, TextScale, TilePicture,
        CLARA_BW_METRICS,
    };

    fn source() -> Source {
        Source {
            handle: "opaque-handle".to_owned(),
            catalog: "montreal".to_owned(),
            catalog_name: "Montréal".to_owned(),
            availability: "available".to_owned(),
            available: true,
        }
    }

    #[test]
    fn panel_ready_cover_is_unpacked_and_rejects_bad_bodies() {
        // PNC1, 3×1, pixels 0, 1 and 15. The last low nibble is padding and
        // must not become a fourth pixel.
        let body = [b'P', b'N', b'C', b'1', 0, 3, 0, 1, 0x01, 0xf0];
        assert_eq!(decode_cover(&body), Some((3, 1, vec![0, 17, 255])));

        for invalid in [
            &body[..7],
            &[b'X', b'N', b'C', b'1', 0, 3, 0, 1, 0x01, 0xf0][..],
            &[b'P', b'N', b'C', b'1', 0, 0, 0, 1, 0x00][..],
            &[b'P', b'N', b'C', b'1', 0, 3, 0, 1, 0x01][..],
        ] {
            assert_eq!(decode_cover(invalid), None);
        }
    }

    /// The read each screen is drawn waiting for. A book's blurb is already in
    /// hand by the time there is a row to open it with, so that screen is the
    /// one that waits for nothing.
    fn waiting_for(view: View) -> Option<super::RequestKind> {
        match view {
            View::Discover => Some(super::RequestKind::Discovery),
            View::Categories => Some(super::RequestKind::CategoryList),
            View::BookstoreCategories => Some(super::RequestKind::BookstoreCategories),
            View::Browse => Some(super::RequestKind::Browse),
            View::Results => Some(super::RequestKind::Search),
            View::Related => Some(super::RequestKind::Related),
            View::Library | View::Holds => Some(super::RequestKind::Books),
            View::Resolve => Some(super::RequestKind::Acknowledge),
            _ => None,
        }
    }

    /// The page height of the panel these tests measure against. Each screen
    /// spends a different amount above its list, so each is asked separately
    /// rather than assuming they agree.
    fn rows() -> usize {
        PretNumerique::default().page_rows()
    }

    fn browse_rows() -> usize {
        PretNumerique::default().browse_rows_per_page()
    }

    fn related_rows() -> usize {
        PretNumerique::default().related_rows_per_page()
    }

    fn out_of_copies() -> Source {
        Source {
            handle: "banq-handle".to_owned(),
            catalog: "banq".to_owned(),
            catalog_name: "BAnQ".to_owned(),
            availability: "On loan".to_owned(),
            available: false,
        }
    }

    fn publication(title: &str, sources: Vec<Source>) -> Publication {
        Publication {
            handle: Some(format!("{title}-handle")),
            pdf_only: false,
            title: title.to_owned(),
            authors: vec!["Fixture author".to_owned()],
            isbn: Some("9780000000000".to_owned()),
            description: None,
            published: None,
            number_of_pages: None,
            publisher: None,
            duration: None,
            goodreads_rating: None,
            goodreads_ratings_count: None,
            goodreads_reviews_count: None,
            sources,
        }
    }

    /// The same book with everything the server can say about it, and a blurb
    /// the length of the longest the libraries publish.
    ///
    /// A short description proves nothing: the screen that shows one is a
    /// screen that pages, and a blurb that fits on one page never turns.
    fn described(book: Publication) -> Publication {
        Publication {
            description: Some(
                "On the desert world Arrakis, Paul Atreides is drawn into a struggle over \
                 power, prophecy and survival, and the spice that every house in the empire \
                 depends on for its ships and its seers. "
                    .repeat(9),
            ),
            published: Some("2019-04-02T22:00:00Z".to_owned()),
            number_of_pages: Some(312),
            publisher: Some("Éditions Québec Amérique".to_owned()),
            duration: Some(33_120),
            goodreads_rating: Some(4.29),
            goodreads_ratings_count: Some(1_700_633),
            goodreads_reviews_count: Some(88_668),
            ..book
        }
    }

    fn book(id: &str, catalog: &str, return_state: Option<&str>) -> Book {
        Book {
            id: id.to_owned(),
            title: format!("Loan {id}"),
            catalog: catalog.to_owned(),
            file_name: "Loan.lcpl".to_owned(),
            return_state: return_state.map(str::to_owned),
        }
    }

    fn job(id: &str, kind: &str, state: &str, book_id: Option<&str>, error: Option<&str>) -> Job {
        Job {
            id: id.to_owned(),
            kind: kind.to_owned(),
            state: state.to_owned(),
            title: format!("Title {id}"),
            catalog: "montreal".to_owned(),
            book_id: book_id.map(str::to_owned),
            error: error.map(str::to_owned),
        }
    }

    fn fetched(commands: Vec<Command>) -> Option<String> {
        commands.into_iter().find_map(|command| match command {
            Command::Spawn {
                work: Task::Fetch { url, .. },
                ..
            } => Some(url),
            _ => None,
        })
    }

    fn posted(commands: Vec<Command>) -> Option<(String, String)> {
        commands.into_iter().find_map(|command| match command {
            Command::Spawn {
                work: Task::Post { url, body, .. },
                ..
            } => Some((url, body)),
            _ => None,
        })
    }

    #[test]
    fn search_parser_keeps_opaque_handles_and_library_sources() {
        let results = parse_search(
            br#"{"results":[{"title":"Book","authors":["Author"],"isbn":"9780000000000","sources":[{"handle":"opaque","catalog":"banq","catalog_name":"BAnQ","availability":"available","is_available":true}]}]}"#,
        )
        .expect("valid search response");
        assert_eq!(results[0].sources[0].handle, "opaque");
        assert_eq!(results[0].sources[0].catalog, "banq");
    }

    #[test]
    fn detail_screen_shows_description_and_goodreads_counts() {
        let result = parse_search(
            br#"{"results":[{"title":"Book","authors":["Author"],"isbn":"9780000000000","description":"A bounded description.","goodreads_rating":4.29,"goodreads_ratings_count":1700633,"goodreads_reviews_count":88668,"sources":[{"handle":"opaque","catalog":"banq","catalog_name":"BAnQ","availability":"available","is_available":true}]}]}"#,
        )
        .expect("valid search response")
        .remove(0);
        let screen = format!(
            "{:?}",
            PretNumerique {
                detail: Some(result),
                ..PretNumerique::default()
            }
            .detail_screen()
        );
        assert!(screen.contains("A bounded description."));
        assert!(screen.contains("4.3 / 5"));
        assert!(screen.contains("1,700,633"));
    }

    #[test]
    fn detail_screen_bounds_long_descriptions_before_library_actions() {
        let result = Publication {
            description: Some("A very long description. ".repeat(40)),
            ..publication("Book", vec![source()])
        };
        let app = PretNumerique {
            detail: Some(result),
            ..PretNumerique::default()
        };
        let screen = format!("{:?}", app.detail_screen());
        assert!(screen.contains("Choose a library"));
        assert!(screen.contains("..."));
        assert!(!screen.contains(&"A very long description. ".repeat(40)));
    }

    #[test]
    fn the_edition_is_drawn_only_where_the_server_named_one() {
        let full = parse_search(
            br#"{"results":[{"title":"Book","authors":["Author"],"published":"2019-04-02T22:00:00Z","number_of_pages":312,"publisher":"  Editions Quebec Amerique  ","sources":[{"handle":"opaque","catalog":"banq","catalog_name":"BAnQ","availability":"available","is_available":true}]}]}"#,
        )
        .expect("valid search response")
        .remove(0);
        assert_eq!(full.published.as_deref(), Some("2019-04-02T22:00:00Z"));
        assert_eq!(full.number_of_pages, Some(312));
        // Trimmed on the way in, so no screen has to think about it.
        assert_eq!(full.publisher.as_deref(), Some("Editions Quebec Amerique"));
        // On a panel with the room for them: a six inch one holds three facts,
        // and which three is the business of the test below.
        let drawn = format!(
            "{:?}",
            PretNumerique {
                metrics: CONTENT_PANELS[1].1,
                detail: Some(full),
                ..PretNumerique::default()
            }
            .detail_screen()
        );
        assert!(drawn.contains("April 2019"), "{drawn}");
        assert!(drawn.contains("312 pages"), "{drawn}");

        // A book the catalogue counted nothing about. Every one of these is a
        // field the server sent, and none of them is a row.
        let bare = parse_search(
            br#"{"results":[{"title":"Book","authors":["Author"],"published":"","number_of_pages":0,"publisher":"   ","duration":0,"sources":[{"handle":"opaque","catalog":"banq","catalog_name":"BAnQ","availability":"available","is_available":true}]}]}"#,
        )
        .expect("valid search response")
        .remove(0);
        assert_eq!(bare.published, None);
        assert_eq!(bare.number_of_pages, None);
        assert_eq!(bare.publisher, None);
        assert_eq!(bare.duration, None);
        let drawn = format!(
            "{:?}",
            PretNumerique {
                detail: Some(bare),
                ..PretNumerique::default()
            }
            .detail_screen()
        );
        for absent in ["Published", "Length", "Publisher", "0 pages"] {
            assert!(!drawn.contains(absent), "{absent} was drawn: {drawn}");
        }
    }

    #[test]
    fn an_audiobook_is_measured_in_hours_and_a_book_in_pages() {
        let listened = |seconds| {
            format!(
                "{:?}",
                PretNumerique {
                    metrics: CONTENT_PANELS[1].1,
                    detail: Some(Publication {
                        duration: Some(seconds),
                        ..publication("Recording", vec![source()])
                    }),
                    ..PretNumerique::default()
                }
                .detail_screen()
            )
        };
        assert!(listened(33_120).contains("9 h 12 min"));
        assert!(listened(2_700).contains("45 min"));
        // An exact two hours says two hours, not two hours and no minutes.
        assert!(listened(7_200).contains("2 h\""), "{}", listened(7_200));
        // A recording has no pages and a book has no running time, so the row
        // that says how much of it there is never says both.
        assert!(!listened(33_120).contains("pages"));
    }

    #[test]
    fn a_publication_date_is_read_as_a_month_and_a_year_or_not_at_all() {
        assert_eq!(
            published_label("2019-04-02T22:00:00Z").as_deref(),
            Some("April 2019")
        );
        assert_eq!(published_label("2019-04-02").as_deref(), Some("April 2019"));
        // All the catalogue knew, and all that is said.
        assert_eq!(published_label("2019").as_deref(), Some("2019"));
        assert_eq!(published_label("2019-13-02").as_deref(), Some("2019"));
        // Nothing that is not a date reaches the panel, whatever the server
        // sends: no row is a better answer than a row of the server's own
        // punctuation.
        for wrong in ["", "soon", "n/a", "-", "0000-01-01", "20190402"] {
            assert_eq!(published_label(wrong), None, "{wrong} was drawn as a date");
        }
    }

    #[test]
    fn a_book_keeps_its_author_and_its_libraries_when_the_facts_do_not_fit() {
        typeset();
        let book = described(publication("Book", vec![source(), out_of_copies()]));
        let tall = PretNumerique {
            metrics: CONTENT_PANELS[1].1,
            detail: Some(book.clone()),
            ..PretNumerique::default()
        };
        let short = PretNumerique {
            metrics: CLARA_BW_METRICS,
            detail: Some(book),
            ..PretNumerique::default()
        };
        // A ten inch panel has room for the whole of it.
        let drawn = format!("{:?}", tall.detail_screen());
        for fact in ["Fixture author", "April 2019", "312 pages", "Éditions"] {
            assert!(drawn.contains(fact), "{fact} is missing: {drawn}");
        }
        // A six inch panel, once the strip carrying the clock is counted, has
        // room for two facts against a book both libraries carry -- and those
        // two are the ones a borrow cannot be made without. A book only one of
        // them carries gets the second library's row back and spends it on
        // facts.
        assert_eq!(short.detail_fact_rows(2), 2);
        assert_eq!(short.detail_fact_rows(1), 4);
        let drawn = format!("{:?}", short.detail_screen());
        assert!(drawn.contains("Fixture author"));
        assert!(drawn.contains("Available now at Montréal"));
        assert!(!drawn.contains("Éditions"), "{drawn}");
        // What was cut was cut from the bottom, not from the middle: the
        // control the reader came for is still on the panel.
        assert!(drawn.contains("Choose a library"));
        assert!(drawn.contains("Read the whole description"));
    }

    #[test]
    fn a_blurb_too_long_for_a_book_screen_is_read_full_screen_and_paged() {
        typeset();
        let mut runner = AppRunner::new(PretNumerique {
            results: vec![described(publication("Book", vec![source()]))],
            ..PretNumerique::default()
        });
        runner.start();
        runner.action(action_id("result.0"));
        assert_eq!(runner.app().view, View::Detail);
        runner.action(action_id(DESCRIPTION));
        assert_eq!(runner.app().view, View::Description);
        let drawn = format!("{:?}", runner.app().screen());
        assert!(drawn.contains("desert world Arrakis"), "{drawn}");
        assert!(drawn.contains("Page 1 of 2"), "{drawn}");
        // The first page cannot go back and the last cannot go on, and both
        // controls are drawn either way.
        assert!(drawn.contains("Previous page"));
        assert!(drawn.contains("Next page"));
        runner.action(action_id(NEXT_PAGE));
        let drawn = format!("{:?}", runner.app().screen());
        assert!(drawn.contains("Page 2 of 2"), "{drawn}");
        runner.action(action_id(NEXT_PAGE));
        assert_eq!(runner.app().description_page, 1, "past the last page");
        runner.action(action_id(PREVIOUS_PAGE));
        assert!(format!("{:?}", runner.app().screen()).contains("Page 1 of 2"));
        // Back is the book, not the list the book was found in.
        runner.action(action_id(BACK));
        assert_eq!(runner.app().view, View::Detail);
        // And opening it again starts at the beginning of it.
        runner.action(action_id(NEXT_PAGE));
        runner.action(action_id(DESCRIPTION));
        assert_eq!(runner.app().description_page, 0);
    }

    /// Every page of the longest blurb the libraries publish, on every panel.
    ///
    /// The screen the matrix draws is the first page, and the first page of a
    /// paginated screen is the one that fits by construction. What can be one
    /// line too tall is the page whose text was measured against a page area
    /// this screen does not have, and what a panel does with that is drop the
    /// controls that would turn it.
    #[test]
    fn no_page_of_a_blurb_is_drawn_past_the_bottom_of_its_panel() {
        typeset();
        // The longest the libraries publish, and then some.
        let blurb = "On the desert world Arrakis, Paul Atreides is drawn into a struggle over \
                     power, prophecy and survival, and the spice that every house in the empire \
                     depends on for its ships and its seers. "
            .repeat(24);
        for (name, metrics) in CONTENT_PANELS {
            for note in [None, Some("The home server would not take this borrow. Look in Library for anything that needs you.".to_owned())] {
                let mut app = PretNumerique {
                    view: View::Description,
                    metrics,
                    note,
                    detail: Some(Publication {
                        description: Some(compact_message(&blurb, super::MAX_DESCRIPTION_CHARS)),
                        ..publication("Book", vec![source()])
                    }),
                    ..PretNumerique::default()
                };
                let pages = app.description_pages().len();
                assert!(pages > 1, "{name}: a blurb that long has to page");
                for page in 0..pages {
                    app.description_page = page;
                    let errors = app
                        .screen()
                        .diagnostics(&metrics, &device_chrome())
                        .issues
                        .into_iter()
                        .filter(|issue| issue.severity == DiagnosticSeverity::Error)
                        .map(|issue| issue.to_string())
                        .collect::<Vec<_>>();
                    assert!(
                        errors.is_empty(),
                        "{name} refused page {page} of {pages}: {}",
                        errors.join("; ")
                    );
                }
            }
        }
    }

    #[test]
    fn a_book_nobody_described_says_so_rather_than_drawing_an_empty_page() {
        typeset();
        let app = PretNumerique {
            view: View::Description,
            detail: Some(publication("Book", vec![source()])),
            ..PretNumerique::default()
        };
        let drawn = format!("{:?}", app.screen());
        assert!(
            drawn.contains("Neither library describes this book"),
            "{drawn}"
        );
        assert!(!drawn.contains("Page 1"), "{drawn}");
    }

    #[test]
    fn search_status_explains_a_partial_catalog_authentication_failure() {
        let note = parse_catalog_status_note(
            br#"{"catalogs":[{"catalog":"montreal","state":"auth_required"},{"catalog":"banq","state":"ready"}]}"#,
        )
        .expect("an authentication status should produce a note");
        assert_eq!(note, "Montréal needs authentication on the proxy.");
    }

    #[test]
    fn library_parser_can_filter_return_state_without_loading_a_book() {
        let books = parse_books(
            br#"[{"id":"book-id","title":"Loan","catalog":"montreal","file_name":"Loan.lcpl","return_state":"return_failed"}]"#,
        )
        .expect("valid books response");
        assert_eq!(books[0].return_state.as_deref(), Some("return_failed"));
        assert_eq!(state_label("return_failed"), "Return failed · loan kept");
    }

    #[test]
    fn job_parser_does_not_require_a_publication_url() {
        let job = parse_job(
            br#"{"id":"job-id","kind":"return","state":"returned","catalog":"banq","title":"Loan"}"#,
        )
        .expect("valid job response");
        assert_eq!(job.kind, "return");
        assert_eq!(job.state, "returned");
        assert_eq!(job.book_id, None);
    }

    #[test]
    fn job_parser_reads_the_book_a_delivery_failure_belongs_to() {
        let jobs = parse_jobs(
            br#"{"jobs":[{"id":"job-id","kind":"import","state":"hook_failed","catalog":"banq","title":"Loan","book_id":"book-id","error_message":"The hook exited with status 1.","acknowledged_at":null,"dedup_key":"import:book-id"}]}"#,
        )
        .expect("valid jobs response");
        assert_eq!(jobs[0].book_id.as_deref(), Some("book-id"));
        assert_eq!(
            jobs[0].error.as_deref(),
            Some("The hook exited with status 1.")
        );
    }

    #[test]
    fn labels_turn_catalog_values_into_reader_copy() {
        assert_eq!(kind_label("return"), "Return");
        assert_eq!(kind_label("import"), "Send");
        assert_eq!(availability_label(&source()), "Available now");
    }

    #[test]
    fn every_job_state_the_server_sends_has_reader_copy() {
        for state in [
            "queued",
            "auth_required",
            "borrowing",
            "downloading",
            "stored",
            "hook_running",
            "complete",
            "hook_failed",
            "failed",
            "borrow_uncertain",
            "returning",
            "returned",
            "return_failed",
            "return_uncertain",
        ] {
            assert_ne!(state_label(state), state, "{state} is shown raw");
        }
    }

    #[test]
    fn authentication_required_jobs_wait_for_an_explicit_refresh() {
        assert!(!is_active_state("auth_required"));
        assert!(is_active_state("queued"));
        assert!(is_active_state("downloading"));
        assert!(is_active_state("returning"));
    }

    #[test]
    fn failure_copy_stays_plain_and_bounded() {
        let long = "The server returned a very long explanation that should stay readable on the panel, and it goes on, and on, and on, and on, and on, and on, without ever becoming an unbounded layout line.";
        let (level, message) =
            settled_message(&job("job-id", "borrow", "failed", None, Some(long)));
        assert_eq!(level, BannerLevel::Attention);
        assert!(message.starts_with("This did not finish, and nothing was borrowed."));
        assert!(message.contains("The server returned a very long explanation"));
        assert!(!message.contains("unbounded layout line"));
        assert!(message.contains("..."));

        assert_eq!(
            unresolved_summary(&job("job-id", "borrow", "borrow_uncertain", None, None)),
            "Montréal · Borrow · Check your library account"
        );
        assert_eq!(
            unresolved_summary(&job("job-id", "borrow", "failed", None, None)),
            "Montréal · Borrow · Did not finish"
        );
        assert_eq!(
            settled_message(&job("job-id", "import", "hook_failed", Some("b"), None)).1,
            "Borrowed, but not sent to your reader."
        );
        assert_eq!(
            settled_message(&job("job-id", "return", "return_failed", Some("b"), None)).1,
            "Return failed · loan kept."
        );
    }

    #[test]
    fn compact_message_collapses_whitespace_and_adds_an_ascii_ellipsis() {
        assert_eq!(compact_message("one\n two   three", 12), "one two t...");
        assert_eq!(compact_message("short", 20), "short");
    }

    #[test]
    fn library_view_keeps_both_catalogues_and_return_confirmation_visible() {
        let mut app = PretNumerique {
            books: vec![Book {
                id: "book-id".to_owned(),
                title: "Fixture loan".to_owned(),
                catalog: "banq".to_owned(),
                file_name: "Fixture.lcpl".to_owned(),
                return_state: None,
            }],
            selected_book: Some(0),
            ..PretNumerique::default()
        };
        let library = format!("{:?}", app.library_screen());
        assert!(library.contains("Montréal"));
        assert!(library.contains("BAnQ"));
        assert!(library.contains("Fixture loan"));
        assert!(library.contains("Ready to return"));
        let confirmation = format!("{:?}", app.confirm_return_screen());
        assert!(confirmation.contains("Return this book?"));
        assert!(confirmation.contains("BAnQ"));
        app.filter = super::CatalogFilter::Montreal;
        assert!(!format!("{:?}", app.library_screen()).contains("Fixture loan"));
    }

    #[test]
    fn the_confirmation_of_a_decision_survives_the_reads_that_follow_it() {
        let mut app = PretNumerique {
            view: View::Resolve,
            jobs: vec![job(
                "uncertain-job",
                "borrow",
                "borrow_uncertain",
                None,
                None,
            )],
            resolve_job: Some("uncertain-job".to_owned()),
            ..PretNumerique::default()
        };
        let mut context = Context::default();
        app.handle_completed(
            &mut context,
            super::RequestKind::Acknowledge,
            br#"{"id":"uncertain-job","kind":"borrow","state":"borrow_uncertain","title":"Title","catalog":"montreal","acknowledged_at":"2026-08-23T13:05:00Z"}"#,
        );
        assert_eq!(app.view, View::Library);
        assert!(app.jobs.is_empty(), "the row goes with the decision");
        assert_eq!(app.queued_request, Some(super::RequestKind::Books));
        let confirmation = app.note.clone().expect("a confirmation");
        assert!(confirmation.starts_with("Cleared."));

        app.handle_completed(&mut context, super::RequestKind::Books, b"[]");
        app.handle_completed(&mut context, super::RequestKind::Jobs, br#"{"jobs":[]}"#);
        assert_eq!(app.note.as_deref(), Some(confirmation.as_str()));
    }

    #[test]
    fn a_resend_that_finished_does_not_claim_a_second_loan() {
        assert_eq!(
            settled_message(&job("job-id", "import", "complete", Some("book-id"), None)).1,
            "Sent to your reader."
        );
        assert_eq!(
            settled_message(&job("job-id", "borrow", "complete", None, None)).1,
            "Borrowed. It is in your Library now."
        );
    }

    #[test]
    fn nothing_is_polled_while_the_reader_is_in_another_application() {
        let mut app = PretNumerique {
            view: View::Detail,
            watch: Some(Watch {
                view: View::Detail,
                job: Some(job("job-id", "borrow", "borrowing", None, None)),
            }),
            ..PretNumerique::default()
        };
        let mut context = Context::default();
        app.schedule_poll(&mut context);
        assert!(app.sleep_task.is_some());
        app.on_background(&mut context);
        assert!(app.sleep_task.is_none(), "the tick is called off");
        // The job is still active, so only coming back may ask again.
        app.on_foreground(&mut context);
        assert_eq!(
            app.inflight.map(|(_, kind)| kind),
            Some(super::RequestKind::Job)
        );
    }

    #[test]
    fn there_is_no_screen_that_lists_jobs() {
        let app = PretNumerique {
            jobs: vec![
                job("job-id", "borrow", "complete", None, None),
                job("other", "return", "returned", None, None),
            ],
            books: vec![book("book-id", "montreal", None)],
            ..PretNumerique::default()
        };
        for drawn in [
            format!("{:?}", app.search_screen()),
            format!("{:?}", app.library_screen()),
            format!("{:?}", app.settings_screen()),
        ] {
            assert!(!drawn.contains("Queue"), "a screen still names a queue");
            assert!(!drawn.contains("Title job-id"), "a job is still listed");
        }
        let nav = format!("{:?}", app.library_screen());
        assert!(nav.contains("Discover"));
        assert!(nav.contains("Library"));
        assert!(nav.contains("Settings"));
    }

    #[test]
    fn borrow_and_return_actions_only_post_to_the_proxy() {
        let result = Publication {
            handle: None,
            ..publication("Fixture title", vec![source()])
        };
        let mut borrow = AppRunner::new(PretNumerique {
            results: vec![result],
            ..PretNumerique::default()
        });
        borrow.start();
        borrow.action(action_id("result.0"));
        assert_eq!(borrow.app().view, View::Detail);
        // One tap: choosing the library is the borrow.
        let commands = borrow.action(action_id("source.0"));
        let drawn = commands
            .iter()
            .find_map(|command| match command {
                Command::SetScreen(screen) => Some(format!("{screen:?}")),
                _ => None,
            })
            .expect("the tap must draw something");
        assert!(
            drawn.contains("Sending the request"),
            "the progress must be on the same frame as the tap: {drawn}"
        );
        let borrow_command = posted(commands).expect("borrow should post a job");
        assert_eq!(borrow_command.0, format!("{API}/jobs"));
        assert!(borrow_command.1.contains("opaque-handle"));
        assert!(!borrow_command.1.contains("https://"));
        // The reader keeps the book they were looking at.
        assert_eq!(borrow.app().view, View::Detail);

        let mut returning = AppRunner::new(PretNumerique {
            view: View::Library,
            books: vec![book("book-id", "banq", None)],
            ..PretNumerique::default()
        });
        returning.start();
        returning.action(action_id("book.0"));
        assert_eq!(returning.app().view, View::ConfirmReturn);
        let return_url = posted(returning.action(action_id(CONFIRM_RETURN)))
            .expect("return should post to the proxy")
            .0;
        assert_eq!(return_url, format!("{API}/books/book-id/return"));
        assert_eq!(returning.app().view, View::Library);
    }

    #[test]
    fn borrow_progress_is_drawn_on_the_book_and_stops_when_it_settles() {
        let mut app = PretNumerique {
            view: View::Detail,
            detail: Some(publication("Fixture title", vec![source()])),
            watch: Some(Watch {
                view: View::Detail,
                job: None,
            }),
            ..PretNumerique::default()
        };
        let mut context = Context::default();
        assert!(format!("{:?}", app.detail_screen()).contains("Sending the request"));

        app.watch = Some(Watch {
            view: View::Detail,
            job: Some(job("job-id", "borrow", "downloading", None, None)),
        });
        assert!(format!("{:?}", app.detail_screen()).contains("Saving book"));
        app.schedule_poll(&mut context);
        assert!(
            app.sleep_task.is_some(),
            "an active job must ask for a tick"
        );

        app.sleep_task = None;
        app.watch = Some(Watch {
            view: View::Detail,
            job: Some(job("job-id", "borrow", "complete", None, None)),
        });
        app.schedule_poll(&mut context);
        assert!(app.sleep_task.is_none(), "a settled job must stop polling");
        let screen = format!("{:?}", app.detail_screen());
        assert!(screen.contains("It is in your Library now."));
        assert!(screen.contains("Open Library"));
    }

    #[test]
    fn a_borrow_the_reader_walked_away_from_stops_polling() {
        let mut app = PretNumerique {
            view: View::Detail,
            watch: Some(Watch {
                view: View::Detail,
                job: Some(job("job-id", "borrow", "borrowing", None, None)),
            }),
            ..PretNumerique::default()
        };
        let mut context = Context::default();
        app.schedule_poll(&mut context);
        assert!(app.sleep_task.is_some());
        app.leave_watch(&mut context, View::Search);
        assert!(app.watch.is_none());
        assert!(app.sleep_task.is_none());
        app.view = View::Search;
        app.poll_watch(&mut context);
        assert!(app.inflight.is_none(), "nothing is asked for after leaving");
    }

    #[test]
    fn a_delivery_failure_is_offered_on_the_loan_it_belongs_to() {
        let mut app = AppRunner::new(PretNumerique {
            view: View::Library,
            books: vec![book("book-id", "montreal", None)],
            jobs: vec![job(
                "hook-job",
                "import",
                "hook_failed",
                Some("book-id"),
                Some("The hook exited with status 1."),
            )],
            ..PretNumerique::default()
        });
        app.start();
        let library = format!("{:?}", app.app().library_screen());
        assert!(library.contains("Not sent to your reader"));
        // Nothing is drawn for it a second time in a section of its own.
        assert!(!library.contains("Needs your attention"));

        app.action(action_id("book.0"));
        assert_eq!(app.app().view, View::Resolve);
        let resolve = format!("{:?}", app.app().resolve_screen());
        assert!(resolve.contains("Send to my reader again"));
        assert!(resolve.contains("both safe on your home server"));
        assert!(resolve.contains("The hook exited with status 1."));

        let retry = posted(app.action(action_id(RETRY_HOOK))).expect("retry should post");
        assert_eq!(retry.0, format!("{API}/jobs/hook-job/retry-hook"));
    }

    #[test]
    fn an_uncertain_borrow_has_a_home_and_a_way_out() {
        let mut app = AppRunner::new(PretNumerique {
            view: View::Library,
            books: vec![book("book-id", "montreal", None)],
            jobs: vec![job(
                "uncertain-job",
                "borrow",
                "borrow_uncertain",
                None,
                Some("Montréal did not confirm the loan."),
            )],
            ..PretNumerique::default()
        });
        app.start();
        let library = format!("{:?}", app.app().library_screen());
        assert!(library.contains("Needs your attention"));
        assert!(library.contains("Check your library account"));

        app.action(action_id("resolve.0"));
        assert_eq!(app.app().view, View::Resolve);
        let resolve = format!("{:?}", app.app().resolve_screen());
        assert!(resolve.contains("cannot tell whether Montréal gave you this loan"));
        assert!(resolve.contains("borrows nothing and returns nothing"));
        assert!(resolve.contains("Montréal did not confirm the loan."));
        assert!(resolve.contains("I checked my account"));

        let acknowledge =
            posted(app.action(action_id(ACKNOWLEDGE))).expect("acknowledge should post");
        assert_eq!(
            acknowledge.0,
            format!("{API}/jobs/uncertain-job/acknowledge")
        );
        assert!(acknowledge.1.is_empty());
    }

    #[test]
    fn an_uncertain_return_is_reachable_from_its_loan() {
        let mut app = AppRunner::new(PretNumerique {
            view: View::Library,
            books: vec![book("book-id", "montreal", Some("return_uncertain"))],
            jobs: vec![job(
                "return-job",
                "return",
                "return_uncertain",
                Some("book-id"),
                Some("Montréal did not confirm the return."),
            )],
            ..PretNumerique::default()
        });
        app.start();
        assert!(format!("{:?}", app.app().library_screen()).contains("Return not confirmed"));
        app.action(action_id("book.0"));
        assert_eq!(app.app().view, View::Resolve);
        let resolve = format!("{:?}", app.app().resolve_screen());
        assert!(resolve.contains("cannot tell whether Montréal took this loan back"));
        assert!(resolve.contains("I checked my account"));
        assert_eq!(
            posted(app.action(action_id(ACKNOWLEDGE)))
                .expect("acknowledge should post")
                .0,
            format!("{API}/jobs/return-job/acknowledge")
        );
    }

    #[test]
    fn the_library_is_quiet_when_nothing_needs_a_person() {
        let app = PretNumerique {
            view: View::Library,
            books: vec![book("book-id", "montreal", None)],
            jobs: vec![
                job("done", "borrow", "complete", None, None),
                job("gone", "return", "returned", Some("other"), None),
            ],
            ..PretNumerique::default()
        };
        let library = format!("{:?}", app.library_screen());
        assert!(!library.contains("Needs your attention"));
        assert!(library.contains("Ready to return"));
    }

    #[test]
    fn a_refused_borrow_says_so_and_reloads_what_needs_a_person() {
        let mut app = PretNumerique::default();
        let mut context = Context::default();
        app.handle_failed(&mut context, super::RequestKind::Borrow, TaskError::Offline);
        let offline = app.note.clone().expect("a note");
        assert!(offline.contains("not on a network"));
        assert!(offline.contains("nothing was borrowed"));

        app.handle_failed(
            &mut context,
            super::RequestKind::Borrow,
            TaskError::NotFound,
        );
        assert!(app
            .note
            .as_deref()
            .is_some_and(|note| note.contains("Look in Library")));
        assert_eq!(app.queued_request, Some(super::RequestKind::Books));

        app.handle_failed(&mut context, super::RequestKind::Return, TaskError::Offline);
        assert!(app
            .note
            .as_deref()
            .is_some_and(|note| note.contains("nothing was returned")));
    }

    #[test]
    fn a_signed_out_catalog_names_the_command_that_fixes_it() {
        let (summary, signed_out) = parse_health(
            br#"{"status":"ok","version":"1","queue_depth":0,"catalogs":[{"catalog":"montreal","state":"auth_required"},{"catalog":"banq","state":"ready"}]}"#,
        )
        .expect("valid health response");
        assert_eq!(summary, "Montréal Sign-in needed · BAnQ Signed in");
        assert_eq!(signed_out, vec!["montreal".to_owned()]);
        let advice = sign_in_advice(&signed_out).expect("advice for a signed-out catalog");
        assert!(advice.contains("PretNumeriqueProxy --auth montreal"));
        assert!(advice.contains("cannot be done from this Kobo"));
        assert_eq!(sign_in_advice(&[]), None);

        let app = PretNumerique {
            view: View::Settings,
            health: Some(summary),
            health_advice: Some(advice),
            ..PretNumerique::default()
        };
        let settings = format!("{:?}", app.settings_screen());
        assert!(settings.contains("Sign-in needed"));
        assert!(settings.contains("--auth montreal"));
    }

    #[test]
    fn a_parked_borrow_points_at_the_only_place_it_can_be_fixed() {
        let app = PretNumerique {
            view: View::Detail,
            detail: Some(publication("Fixture title", vec![source()])),
            watch: Some(Watch {
                view: View::Detail,
                job: Some(job(
                    "auth-job",
                    "borrow",
                    "auth_required",
                    None,
                    Some("Montréal needs authentication on the proxy."),
                )),
            }),
            ..PretNumerique::default()
        };
        let screen = format!("{:?}", app.detail_screen());
        assert!(screen.contains("has to sign in to Montréal again"));
        assert!(screen.contains("Open Settings"));
    }

    #[test]
    fn resolve_copy_covers_every_state_that_reaches_that_screen() {
        for state in [
            "hook_failed",
            "borrow_uncertain",
            "return_uncertain",
            "return_failed",
            "failed",
        ] {
            let explanation = resolve_explanation(&job("job-id", "borrow", state, None, None));
            assert!(!explanation.is_empty(), "{state} has no explanation");
            assert!(!explanation.contains(state), "{state} leaks its raw name");
        }
    }

    #[test]
    fn state_store_keeps_the_query_and_favorites() {
        let mut app = PretNumerique::default();
        let mut context = Context::default();
        app.on_start(&mut context);
        app.on_store(
            &mut context,
            StoreResult::Loaded {
                key: "ui-state".to_owned(),
                // The trailing line is a job ID an older build wrote and
                // nothing ever read.
                value: Some(b"Dune\njob-1\n".to_vec()),
            },
        );
        assert!(app.loaded_state);
        assert_eq!(app.query, "Dune");
        assert_eq!(app.state_bytes(), b"Dune\n\n");
    }

    /// Every screen this app can be looked at, except the one it draws on its
    /// way out and the keyboard, which carries its own Cancel.
    const VIEWS: [View; 15] = [
        View::Discover,
        View::Categories,
        View::BookstoreCategories,
        View::Browse,
        View::Filters,
        View::Search,
        View::Results,
        View::Detail,
        View::Description,
        View::Related,
        View::Library,
        View::Holds,
        View::ConfirmReturn,
        View::Resolve,
        View::Settings,
    ];

    /// The chrome a real device draws this app with.
    ///
    /// `kobod` builds it in `chrome_for`: no Back, because a menu entry presents
    /// this app directly and it is therefore at home, and a status band on every
    /// screen, because the band is withheld only from a reading screen and this
    /// app has none. The band is laid out above everything else and takes 59
    /// pixels of a Clara's content with it.
    ///
    /// Every assertion below used `Chrome::with_back(false)`, which draws no
    /// band. So the whole matrix measured a screen 59 pixels taller than any
    /// device has, agreed with the app's own arithmetic -- which was making the
    /// same mistake -- and passed while the browse page controls were sheared
    /// off by the nav bar on a real Clara. There is nothing to gain from also
    /// measuring without the band: a screen that fits the tighter chrome fits
    /// the looser one, and this app never gets the looser one.
    fn device_chrome() -> Chrome {
        Chrome::measuring(false)
    }

    /// The real typeface, installed before anything is measured.
    ///
    /// Without it every layout assertion is made against the built-in bitmap
    /// fallback, which is fixed-width and uppercase-only: a screen that fits
    /// there is nothing like a screen that fits on the panel, and the whole
    /// claim these tests make rests on this call. `AppRunner` installs it too,
    /// so a test that never builds one was measuring whatever the test that ran
    /// before it happened to leave behind.
    fn typeset() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            let _ = kobo_text::install(CLARA_BW_METRICS);
        });
    }

    /// The panels whose content these tests measure.
    ///
    /// Not the Nia, and not because it does not matter. The typesetter is
    /// installed once per process and cannot be swapped, so every panel here is
    /// measured with the type chosen for a Clara. On a larger panel that is
    /// conservative and the assertion holds; on the Nia, whose pixels are
    /// coarser, it sets every word about forty per cent too large and would
    /// fail screens that draw perfectly there. Its bar is still checked on
    /// every screen below, which is geometry rather than type.
    const CONTENT_PANELS: [(&str, DisplayMetrics); 2] = [
        ("clara-bw", CLARA_BW_METRICS),
        (
            "elipsa-2e",
            DisplayMetrics {
                width: 1404,
                height: 1872,
                pixels_per_inch: 227,
                text_scale: TextScale::Default,
            },
        ),
    ];

    const PANELS: [(&str, DisplayMetrics); 3] = [
        ("clara-bw", CLARA_BW_METRICS),
        (
            "nia",
            DisplayMetrics {
                width: 758,
                height: 1024,
                pixels_per_inch: 212,
                text_scale: TextScale::Default,
            },
        ),
        (
            "elipsa-2e",
            DisplayMetrics {
                width: 1404,
                height: 1872,
                pixels_per_inch: 227,
                text_scale: TextScale::Default,
            },
        ),
    ];

    /// Every screen with something on it, and every list long enough to page.
    ///
    /// A screen drawn empty proves nothing about whether it fits: the page that
    /// breaks a panel is the full one.
    ///
    /// The content here is the content off a real device, because content that
    /// is merely plausible proves nothing either. The heading was
    /// "Books by Marilou Addison", which sets on one line; the subject that
    /// broke the browse screen was "Enfant, adolescent et enseignement", which
    /// sets on two. The catalogue offered two orders in English, which fit one
    /// row of chips; it offers three in French, which with the PDF filter
    /// beside them are two rows. Every title carried one author; the list that
    /// wrapped carried five. None of the three was a rounding error and all
    /// three were absent from this fixture.
    #[allow(clippy::too_many_lines, reason = "one screenful of fixture data")]
    fn populated() -> PretNumerique {
        let many = |count: usize| {
            (0..count)
                .map(|index| Publication {
                    authors: vec![
                        "Jocelyn Boisvert".to_owned(),
                        "Julie Champagne".to_owned(),
                        "Denis Côté".to_owned(),
                        "Alexandre Côté-Fournier".to_owned(),
                        "Pierrette Dubé".to_owned(),
                    ],
                    goodreads_rating: Some(4.1),
                    ..publication(
                        &format!("A fixture title long enough to wrap number {index}"),
                        vec![source(), out_of_copies()],
                    )
                })
                .collect::<Vec<_>>()
        };
        PretNumerique {
            results: many(9),
            groups: vec![
                Group {
                    title: "Recent releases".to_owned(),
                    category: Some("recent".to_owned()),
                    total: Some(3204),
                    publications: many(4),
                },
                Group {
                    title: "En août, je lis québécois".to_owned(),
                    category: Some("aout".to_owned()),
                    total: Some(18),
                    publications: many(4),
                },
                Group {
                    title: "Littérature LGBTQ2+".to_owned(),
                    category: None,
                    total: None,
                    publications: many(2),
                },
            ],
            categories: (0..9)
                .map(|index| Category {
                    name: format!("Enfant, adolescent et enseignement {index}"),
                    key: format!("Y{index}"),
                    libraries: vec!["Montréal".to_owned(), "BAnQ".to_owned()],
                    total: Some(11594),
                })
                .collect(),
            browse: BrowseQuery {
                title: "Enfant, adolescent et enseignement".to_owned(),
                category: Some("YB".to_owned()),
                audience: None,
                author: None,
                sort: Some("rating_desc".to_owned()),
                page: 2,
                available_only: false,
                bookstore: false,
            },
            browsed: many(9),
            sorts: vec![
                Sort {
                    key: "created_at_desc".to_owned(),
                    label: "Acquisitions récentes".to_owned(),
                },
                Sort {
                    key: "issued_on_desc".to_owned(),
                    label: "Parutions récentes".to_owned(),
                },
                Sort {
                    key: "rating_desc".to_owned(),
                    label: "Highest rated".to_owned(),
                },
            ],
            browse_paging: Paging {
                page: 2,
                total_pages: Some(3),
                has_previous: true,
                has_next: true,
            },
            browse_number: 2,
            related: many(8),
            detail: Some(described(publication(
                "Fixture title",
                vec![source(), out_of_copies()],
            ))),
            books: (0..8)
                .map(|index| book(&format!("book-{index}"), "montreal", None))
                .collect(),
            shelf: (0..8)
                .map(|index| ShelfEntry {
                    title: format!("Mûre secrète et melon rafraîchissant {index}"),
                    authors: vec!["Sandra Verilli".to_owned()],
                    catalog: "montreal".to_owned(),
                    kind: "hold".to_owned(),
                    since: Some("2026-08-24T01:05:50.211Z".to_owned()),
                    until: Some("2026-10-24T00:57:57.431Z".to_owned()),
                    position: Some(7),
                    total: Some(7),
                })
                .collect(),
            selected_book: Some(0),
            jobs: vec![job("job-id", "borrow", "borrow_uncertain", None, None)],
            resolve_job: Some("job-id".to_owned()),
            ..PretNumerique::default()
        }
    }

    #[test]
    fn the_way_back_to_the_reader_is_on_every_screen_of_every_panel() {
        typeset();
        // A menu entry presents this app directly, which makes it home, and a
        // home application is drawn with no back control of the runtime's own.
        // So this bar slot is the only way out short of a power cycle, and the
        // layout engine drops what does not fit in silence.
        let reader = action_id(READER);
        for (name, metrics) in PANELS {
            for view in VIEWS {
                let app = PretNumerique {
                    view,
                    ..populated()
                };
                let screen = app.screen();
                let layout = screen.layout_with(&metrics, &device_chrome());
                let slot = layout.nodes.iter().find(|node| {
                    matches!(
                        node.kind,
                        LayoutKind::NavDestination(action, ..)
                            | LayoutKind::NavDestinationSelected(action, ..)
                            if action == reader
                    )
                });
                let slot = slot.unwrap_or_else(|| {
                    panic!("{name}: no way back to the reader on {view:?}");
                });
                assert!(
                    slot.rect.width >= metrics.touch_target_minimum(),
                    "{name}: the way out is {} wide on {view:?}",
                    slot.rect.width
                );
                // A label wider than its slot is drawn over its neighbours,
                // which is how two words become unreadable rather than absent.
                assert!(
                    !screen
                        .diagnostics(&metrics, &device_chrome())
                        .issues
                        .iter()
                        .any(|issue| issue.kind == LayoutIssueKind::TextOverflow
                            && issue.node == Some(slot.id)),
                    "{name}: the way out does not fit its slot on {view:?}"
                );
            }
        }
    }

    /// The renderer's own verdict on every screen, on every panel.
    ///
    /// An error means something was clipped, dropped below the fold, or drawn
    /// too small to hit, and none of those are visible in a screenshot until
    /// somebody notices what is missing. A chip 79 pixels wide against an 83
    /// pixel minimum got all the way to the panel once.
    #[test]
    fn every_screen_is_drawn_cleanly_on_every_panel() {
        typeset();
        for (name, metrics) in CONTENT_PANELS {
            for view in VIEWS {
                let app = PretNumerique {
                    view,
                    metrics,
                    ..populated()
                };
                let errors = app
                    .screen()
                    .diagnostics(&metrics, &device_chrome())
                    .issues
                    .into_iter()
                    .filter(|issue| issue.severity == DiagnosticSeverity::Error)
                    .map(|issue| issue.to_string())
                    .collect::<Vec<_>>();
                assert!(
                    errors.is_empty(),
                    "{name} refused {view:?}: {}",
                    errors.join("; ")
                );
            }
        }
    }

    #[test]
    fn a_detail_cover_does_not_push_the_library_choices_off_the_panel() {
        typeset();
        for (name, metrics) in CONTENT_PANELS {
            let app = PretNumerique {
                view: View::Detail,
                detail: Some(described(publication("Cover fixture", vec![source()]))),
                cover: Some(TilePicture::new(super::PictureHandle(41), 260, 419)),
                metrics,
                ..PretNumerique::default()
            };
            let errors = app
                .screen()
                .diagnostics(&metrics, &device_chrome())
                .issues
                .into_iter()
                .filter(|issue| issue.severity == DiagnosticSeverity::Error)
                .map(|issue| issue.to_string())
                .collect::<Vec<_>>();
            assert!(
                errors.is_empty(),
                "{name} refused a cover: {}",
                errors.join("; ")
            );
        }
    }

    #[test]
    fn bookstore_category_rows_and_favorites_fit_every_panel() {
        typeset();
        for (name, metrics) in CONTENT_PANELS {
            let app = PretNumerique {
                view: View::BookstoreCategories,
                bookstore_categories: (0..5)
                    .map(|index| BookstoreCategory {
                        id: format!("{index}"),
                        name: format!("Category {index}"),
                        path: format!("Jeunesse / Category {index}"),
                        held: 16,
                        bookstore_total: Some(1000),
                    })
                    .collect(),
                metrics,
                ..PretNumerique::default()
            };
            let errors = app
                .screen()
                .diagnostics(&metrics, &device_chrome())
                .issues
                .into_iter()
                .filter(|issue| issue.severity == DiagnosticSeverity::Error)
                .map(|issue| issue.to_string())
                .collect::<Vec<_>>();
            assert!(
                errors.is_empty(),
                "{name} refused bookstore categories: {}",
                errors.join("; ")
            );
        }
    }

    /// What a page of each list actually holds on the panel this app ships to.
    ///
    /// Written down so that a change to any of the numbers above has to be
    /// argued for rather than noticed later on a device. A browse page held
    /// four books and could not be turned; it holds six and can.
    #[test]
    fn a_page_of_each_list_holds_what_the_panel_has_room_for() {
        typeset();
        let app = PretNumerique {
            metrics: CLARA_BW_METRICS,
            ..populated()
        };
        assert_eq!(app.content_height(), 1131, "a Clara 2E, less the clock");
        assert_eq!(app.browse_rows_per_page(), 6);
        assert_eq!(app.related_rows_per_page(), 6);
        assert_eq!(app.results_rows_per_page(), 5);
        assert_eq!(app.page_rows(), 5);
        assert_eq!(app.holds_rows_per_page(), 5);
    }

    /// Both controls that turn a page, whole and inside the content area, on
    /// every screen that pages and every panel.
    ///
    /// This is the assertion that was missing, and the reason nothing caught a
    /// browse screen whose buttons were 40 per cent visible on a real Clara.
    /// The two matrix tests above would have caught them being *dropped* --
    /// `ContentOverflow` is an error and they check for errors -- but the
    /// renderer had drawn them, three fifths of the way under the navigation
    /// bar, and `Clipped` did not fire either because the whole-matrix chrome
    /// carried no status band and so the buttons really did fit the screen the
    /// tests were measuring. Both halves had to be wrong at once, which is why
    /// this test checks the geometry of these two controls by name rather than
    /// trusting an absence of complaints.
    ///
    /// E Ink does not scroll. A list whose page cannot be turned is a list
    /// whose later pages do not exist.
    #[test]
    fn the_page_controls_are_whole_on_every_paging_screen_and_panel() {
        typeset();
        let paging = [
            View::Discover,
            View::Categories,
            View::Browse,
            View::Filters,
            View::Results,
            View::Description,
            View::Related,
            View::Library,
            View::Holds,
        ];
        for (name, metrics) in CONTENT_PANELS {
            for view in paging {
                let app = PretNumerique {
                    view,
                    metrics,
                    ..populated()
                };
                let screen = app.screen();
                let layout = screen.layout_with(&metrics, &device_chrome());
                for (action, label) in [(PREVIOUS_PAGE, "Previous page"), (NEXT_PAGE, "Next page")]
                {
                    let wanted = action_id(action);
                    let node = layout
                        .nodes
                        .iter()
                        .find(|node| {
                            matches!(node.kind, LayoutKind::Button(drawn, ..) if drawn == wanted)
                        })
                        .unwrap_or_else(|| panic!("{name}: no {label} on {view:?}"));
                    let bottom = layout.content.y + layout.content.height;
                    assert!(
                        node.rect.y >= layout.content.y && node.rect.y + node.rect.height <= bottom,
                        "{name}: {label} on {view:?} is drawn from {} to {}, \
                         outside the content area {} to {bottom}",
                        node.rect.y,
                        node.rect.y + node.rect.height,
                        layout.content.y,
                    );
                    assert!(
                        node.rect.height >= metrics.touch_target_minimum(),
                        "{name}: {label} on {view:?} is {} tall",
                        node.rect.height
                    );
                }
            }
        }
    }

    /// Every filter the screen offers can be reached on some page of it.
    ///
    /// The chips this replaced were lost two ways at once: a reply carrying
    /// fewer facets took the PDF toggle with it, and a wrapped run of them
    /// pushed the page controls off the panel. Neither is possible now, but a
    /// fifth group would page, and a group on a page nobody can turn to is the
    /// same fault wearing different clothes.
    #[test]
    fn every_filter_is_reachable_on_some_page_of_the_filters_screen() {
        typeset();
        for (name, metrics) in CONTENT_PANELS {
            for sorts in [Vec::new(), populated().sorts] {
                let app = PretNumerique {
                    view: View::Filters,
                    metrics,
                    sorts,
                    ..populated()
                };
                let groups = app.filter_groups();
                let per_page = app.filter_groups_per_page(&groups);
                let pages = groups.len().div_ceil(per_page.max(1));
                let mut seen = 0;
                for page in 0..pages {
                    let app = PretNumerique {
                        filters_page: page,
                        ..PretNumerique {
                            view: View::Filters,
                            metrics,
                            sorts: app.sorts.clone(),
                            ..populated()
                        }
                    };
                    let drawn = format!("{:?}", app.filters_screen());
                    for group in super::slice_of(&groups, page, per_page) {
                        seen += 1;
                        assert!(
                            drawn.contains(group.title),
                            "{name}: {} is not on page {page}",
                            group.title
                        );
                        for (_, label, _) in &group.chips {
                            let label: &str = label;
                            assert!(
                                drawn.contains(label),
                                "{name}: {label} is not on page {page}"
                            );
                        }
                    }
                }
                assert_eq!(seen, groups.len(), "{name}: a group has no page");
                // The one control that is never the catalogue's is always here.
                let anywhere = (0..pages).any(|page| {
                    format!(
                        "{:?}",
                        PretNumerique {
                            view: View::Filters,
                            metrics,
                            filters_page: page,
                            sorts: app.sorts.clone(),
                            ..populated()
                        }
                        .filters_screen()
                    )
                    .contains("Include PDF")
                });
                assert!(anywhere, "{name}: the PDF filter is gone");
            }
        }
    }

    /// The same, for a screen with nothing on it yet: an empty list, a wait, or
    /// a note across the foot of it.
    #[test]
    fn the_states_a_screen_can_be_in_are_drawn_cleanly() {
        typeset();
        for (name, metrics) in CONTENT_PANELS {
            for view in VIEWS {
                for (state, app) in [
                    (
                        "empty",
                        PretNumerique {
                            view,
                            metrics,
                            note: Some(
                                "A note long enough to take a line of its own on a narrow panel."
                                    .to_owned(),
                            ),
                            ..PretNumerique::default()
                        },
                    ),
                    (
                        "waiting",
                        PretNumerique {
                            view,
                            metrics,
                            inflight: waiting_for(view).map(|kind| (kobo_sdk::TaskId(1), kind)),
                            ..PretNumerique::default()
                        },
                    ),
                ] {
                    let errors = app
                        .screen()
                        .diagnostics(&metrics, &device_chrome())
                        .issues
                        .into_iter()
                        .filter(|issue| issue.severity == DiagnosticSeverity::Error)
                        .map(|issue| issue.to_string())
                        .collect::<Vec<_>>();
                    assert!(
                        errors.is_empty(),
                        "{name} refused a {state} {view:?}: {}",
                        errors.join("; ")
                    );
                }
            }
        }
    }

    /// A book's screen with everything it can carry at once: a request in
    /// flight, a description, a rating, two libraries, two authors and its
    /// neighbours.
    ///
    /// The panel loses whatever does not fit without saying so, and the note is
    /// drawn where a borrow that failed reports itself, so this is the
    /// arrangement that has to hold.
    #[test]
    fn the_fullest_book_screen_still_fits_every_panel() {
        typeset();
        let book = Publication {
            authors: vec![
                "Marilou Addison".to_owned(),
                "Geneviève Cloutier".to_owned(),
            ],
            description: Some(
                "On the desert world Arrakis, Paul Atreides is drawn into a struggle over \
                 power, prophecy and survival, and the spice that every house in the empire \
                 depends on for its ships and its seers."
                    .to_owned(),
            ),
            goodreads_rating: Some(4.29),
            goodreads_ratings_count: Some(1_700_633),
            goodreads_reviews_count: Some(88_668),
            ..publication("Qui va séduire Henry ?", vec![source(), out_of_copies()])
        };
        let busy = PretNumerique {
            view: View::Detail,
            detail: Some(book.clone()),
            note: Some(
                "This Kobo is not on a network, so nothing was borrowed. Join Wi-Fi and try again."
                    .to_owned(),
            ),
            watch: Some(Watch {
                view: View::Detail,
                job: Some(job("borrow-job", "borrow", "downloading", None, None)),
            }),
            ..PretNumerique::default()
        };
        let quiet = PretNumerique {
            view: View::Detail,
            detail: Some(book),
            ..PretNumerique::default()
        };
        let drawn = format!("{:?}", busy.detail_screen());
        assert!(drawn.contains("not on a network"), "the note must be drawn");
        assert!(drawn.contains("Saving book"));
        assert!(drawn.contains("Choose a library"));
        // The blurb and the ways on give up their room while there is something
        // more urgent to read, and come back when there is not.
        assert!(!drawn.contains("desert world Arrakis"));
        assert!(!drawn.contains("More by this author"));
        let quiet_drawn = format!("{:?}", quiet.detail_screen());
        assert!(quiet_drawn.contains("desert world Arrakis"));
        assert!(quiet_drawn.contains("More by this author"));
        assert!(quiet_drawn.contains("Similar"));

        for (name, metrics) in CONTENT_PANELS {
            // The runtime tells the app which panel it is drawing to, and how
            // much of a book's screen is optional follows from that.
            for (state, screen) in [
                (
                    "busy",
                    PretNumerique {
                        metrics,
                        detail: busy.detail.clone(),
                        note: busy.note.clone(),
                        watch: busy.watch.clone(),
                        ..PretNumerique::default()
                    }
                    .detail_screen(),
                ),
                (
                    "quiet",
                    PretNumerique {
                        metrics,
                        detail: quiet.detail.clone(),
                        ..PretNumerique::default()
                    }
                    .detail_screen(),
                ),
            ] {
                let errors = screen
                    .diagnostics(&metrics, &device_chrome())
                    .issues
                    .into_iter()
                    .filter(|issue| issue.severity == DiagnosticSeverity::Error)
                    .map(|issue| issue.to_string())
                    .collect::<Vec<_>>();
                assert!(
                    errors.is_empty(),
                    "{name} refused a {state} book screen: {}",
                    errors.join("; ")
                );
            }
        }
    }

    /// A book with every fact the server can send, on every panel, however many
    /// libraries carry it.
    ///
    /// The matrix below draws one arrangement of this screen. What the fact
    /// budget rations is a block whose height depends on the book rather than
    /// the panel, and the arrangement it is easiest to get wrong is the one it
    /// has the most room for: a book one library carries, where three facts
    /// become five and the room they came from was the second library's row.
    #[test]
    fn a_book_the_server_knows_everything_about_fits_every_panel() {
        typeset();
        for (name, metrics) in CONTENT_PANELS {
            for sources in [vec![source()], vec![source(), out_of_copies()]] {
                let libraries = sources.len();
                let app = PretNumerique {
                    view: View::Detail,
                    metrics,
                    detail: Some(Publication {
                        pdf_only: true,
                        authors: vec![
                            "Marilou Addison".to_owned(),
                            "Geneviève Cloutier".to_owned(),
                        ],
                        ..described(publication("Qui va séduire Henry ?", sources))
                    }),
                    ..PretNumerique::default()
                };
                let errors = app
                    .screen()
                    .diagnostics(&metrics, &device_chrome())
                    .issues
                    .into_iter()
                    .filter(|issue| issue.severity == DiagnosticSeverity::Error)
                    .map(|issue| issue.to_string())
                    .collect::<Vec<_>>();
                assert!(
                    errors.is_empty(),
                    "{name} refused a book at {libraries} libraries: {}",
                    errors.join("; ")
                );
            }
        }
    }

    /// Library on its worst day: two things needing a person, a queue, a loan
    /// the home server has never seen, and more loans than fit.
    ///
    /// Every section above the list is conditional, so this is the arrangement
    /// no other test produces and the one that overflows first.
    #[test]
    fn the_busiest_library_screen_still_fits_every_panel() {
        typeset();
        let app = PretNumerique {
            view: View::Library,
            books: (0..4)
                .map(|index| book(&format!("book-{index}"), "montreal", None))
                .collect(),
            jobs: vec![
                job("uncertain-borrow", "borrow", "borrow_uncertain", None, None),
                job("uncertain-hold", "hold", "hold_uncertain", None, None),
            ],
            shelf: vec![
                ShelfEntry {
                    title: "Borrowed on the library website".to_owned(),
                    authors: Vec::new(),
                    catalog: "montreal".to_owned(),
                    kind: "loan".to_owned(),
                    since: None,
                    until: Some("2026-09-12".to_owned()),
                    position: None,
                    total: None,
                },
                ShelfEntry {
                    title: "Mûre secrète et melon rafraîchissant".to_owned(),
                    authors: vec!["Sandra Verilli".to_owned()],
                    catalog: "montreal".to_owned(),
                    kind: "hold".to_owned(),
                    since: Some("2026-08-24T01:05:50.211Z".to_owned()),
                    until: Some("2026-10-24T00:57:57.431Z".to_owned()),
                    position: Some(7),
                    total: Some(7),
                },
            ],
            ..PretNumerique::default()
        };
        let drawn = format!("{:?}", app.library_screen());
        assert!(drawn.contains("Needs your attention"));
        assert!(drawn.contains("On hold"));
        assert!(drawn.contains("not on your home server"));
        for (name, metrics) in CONTENT_PANELS {
            let errors = app
                .library_screen()
                .diagnostics(&metrics, &device_chrome())
                .issues
                .into_iter()
                .filter(|issue| issue.severity == DiagnosticSeverity::Error)
                .map(|issue| issue.to_string())
                .collect::<Vec<_>>();
            assert!(
                errors.is_empty(),
                "{name} refused a busy Library: {}",
                errors.join("; ")
            );
        }
    }

    #[test]
    fn discover_is_the_first_screen_and_search_is_one_tap_from_it() {
        let mut app = AppRunner::new(PretNumerique::default());
        let commands = app.start();
        assert_eq!(app.app().view, View::Discover);
        // The home page is read straight away: an empty first screen would be
        // the app's answer to being opened.
        assert!(commands.iter().any(|command| matches!(
            command,
            Command::Spawn {
                work: Task::Fetch { url, .. },
                ..
            } if url == &format!("{API}/discovery")
        )));

        let drawn = format!("{:?}", app.app().discover_screen());
        assert!(drawn.contains("Discover"), "the bar must mark where we are");
        assert!(drawn.contains("Search"));
        assert!(drawn.contains("Categories"));

        app.action(action_id(SEARCH));
        assert_eq!(app.app().view, View::Search);
        app.action(action_id(BACK));
        assert_eq!(app.app().view, View::Discover);
    }

    #[test]
    fn discovery_merges_groups_and_names_the_library_that_has_a_copy() {
        let groups = parse_groups(
            r#"{"groups":[{"title":"Recent releases","category":"recent","total":3204,"publications":[
                {"handle":"opaque-book","title":"Les triplettes","authors":[{"name":"Ariane Michaud"}],
                 "sources":[{"handle":"m","catalog":"montreal","catalog_name":"Montréal","availability":"On loan","is_available":false},
                            {"handle":"b","catalog":"banq","catalog_name":"BAnQ","availability":"Available now","is_available":true}]}]}]}"#.as_bytes(),
        )
        .expect("a readable home page");
        assert_eq!(groups[0].title, "Recent releases");
        assert_eq!(groups[0].publications[0].authors, vec!["Ariane Michaud"]);
        // Out at one library and free at the other is available, and says
        // which one has it.
        assert_eq!(
            availability_summary(&groups[0].publications[0]),
            "Available now at BAnQ"
        );

        let app = PretNumerique {
            view: View::Discover,
            groups,
            ..PretNumerique::default()
        };
        let drawn = format!("{:?}", app.discover_screen());
        assert!(drawn.contains("Recent releases"));
        assert!(drawn.contains("Les triplettes"));
        // A row says whether it can be had, and no more: which library is the
        // book screen's business, because that is where a library is chosen.
        assert!(drawn.contains("A. Michaud · Available"));
        assert!(!drawn.contains("Available now at BAnQ"));
        assert!(drawn.contains("See the whole list"));
        assert!(drawn.contains("3,204 books in this list"));
    }

    /// A list row has one line under the title, and these are the names that
    /// have to fit on it. Every one is a real form from these two catalogues.
    #[test]
    fn a_list_row_names_the_first_author_and_counts_the_rest() {
        for (whole, short) in [
            ("David Suzuki", "D. Suzuki"),
            ("Jocelyn Boisvert", "J. Boisvert"),
            // A hyphenated family name is one name and keeps both halves.
            ("Alexandre Côté-Fournier", "A. Côté-Fournier"),
            // A hyphenated given name is two, and both initials are kept:
            // "M. Larochelle" cannot tell Marie-Ève from Marc-Antoine.
            ("Marie-Ève Larochelle", "M.-È. Larochelle"),
            ("Maude Nepveu-Villeneuve", "M. Nepveu-Villeneuve"),
            // An accented capital is upper case already and stays one
            // character.
            ("Émilie Ouellet", "É. Ouellet"),
            ("Ôta Yôko", "Ô. Yôko"),
            // A particle belongs to the family name.
            ("Jean de la Fontaine", "J. de la Fontaine"),
            ("Antoine de Saint-Exupéry", "A. de Saint-Exupéry"),
            // Two given names, two initials.
            ("Jean Paul Sartre", "J. P. Sartre"),
            // Nothing to shorten around, or nothing gained by shortening.
            ("Molière", "Molière"),
            ("Ed Yong", "Ed Yong"),
            ("Li Bo", "Li Bo"),
            ("de la Fontaine", "de la Fontaine"),
            ("", ""),
        ] {
            assert_eq!(
                super::shortened_name(whole),
                short,
                "{whole} was shortened wrongly"
            );
        }
        let five = [
            "Jocelyn Boisvert",
            "Julie Champagne",
            "Denis Côté",
            "Alexandre Côté-Fournier",
            "Pierrette Dubé",
        ]
        .map(str::to_owned);
        assert_eq!(super::short_author_line(&five), "J. Boisvert +4");
        assert_eq!(super::short_author_line(&five[..1]), "J. Boisvert");
        assert_eq!(super::short_author_line(&[]), "Unknown author");
        // The book's own screen still names every one of them.
        assert!(super::author_line(&five).contains("Pierrette Dubé"));
    }

    /// A row says whether a book can be had. Which library has it is the book
    /// screen's business, because the book screen is where one is chosen.
    #[test]
    fn a_list_row_says_only_whether_a_book_can_be_had() {
        let free = publication("Free", vec![source(), out_of_copies()]);
        let out = publication("Out", vec![out_of_copies()]);
        let nowhere = Publication {
            sources: Vec::new(),
            ..publication("Nowhere", vec![])
        };
        assert_eq!(super::availability_state(&free), "Available");
        assert_eq!(super::availability_state(&out), "All copies out");
        assert_eq!(super::availability_state(&nowhere), "Not available");
        assert!(!publication_summary(&free).contains("Montréal"));
        // And the book's own screen still names every library and its state.
        let drawn = format!(
            "{:?}",
            PretNumerique {
                view: View::Detail,
                detail: Some(free),
                ..PretNumerique::default()
            }
            .detail_screen()
        );
        assert!(drawn.contains("Available now at Montréal"));
        assert!(drawn.contains("Montréal"));
        assert!(drawn.contains("BAnQ"));
    }

    #[test]
    fn availability_is_the_best_answer_across_the_libraries() {
        let both_out = publication("Out everywhere", vec![out_of_copies()]);
        assert!(!both_out.is_available());
        assert_eq!(availability_summary(&both_out), "Every copy is out at BAnQ");
        let mixed = publication("Out at one", vec![out_of_copies(), source()]);
        assert!(mixed.is_available());
        assert_eq!(availability_summary(&mixed), "Available now at Montréal");
        let everywhere = publication(
            "Free everywhere",
            vec![
                source(),
                Source {
                    catalog: "banq".to_owned(),
                    catalog_name: "BAnQ".to_owned(),
                    available: true,
                    ..out_of_copies()
                },
            ],
        );
        assert_eq!(
            availability_summary(&everywhere),
            "Available now at Montréal and BAnQ"
        );
    }

    #[test]
    fn a_page_turn_stops_at_both_ends() {
        // One page: nowhere to go in either direction.
        let single = Paging::sized(0, rows() - 1, rows());
        assert!(!single.has_previous && !single.has_next);
        assert_eq!(single.total_pages, Some(1));
        // Three pages, walked from the first to the last and back.
        let three = rows() * 2 + 1;
        let first = Paging::sized(0, three, rows());
        assert!(!first.has_previous && first.has_next);
        let middle = Paging::sized(1, three, rows());
        assert!(middle.has_previous && middle.has_next);
        let last = Paging::sized(2, three, rows());
        assert!(last.has_previous && !last.has_next);
        assert_eq!(last.total_pages, Some(3));
        // An exact multiple of the page must not offer an empty page after it.
        assert!(!Paging::sized(1, rows() * 2, rows()).has_next);
        // Nothing at all is one empty page, not none.
        assert_eq!(Paging::sized(0, 0, rows()).total_pages, Some(1));

        assert_eq!(turned(0, false, three, rows()), 0);
        assert_eq!(turned(2, true, three, rows()), 2);
        assert_eq!(turned(0, true, three, rows()), 1);
        assert_eq!(turned(0, true, 0, rows()), 0);
    }

    #[test]
    fn the_page_controls_are_drawn_disabled_rather_than_taken_away() {
        let app = PretNumerique {
            view: View::Results,
            results: (0..rows() - 1)
                .map(|index| publication(&format!("Title {index}"), vec![source()]))
                .collect(),
            ..PretNumerique::default()
        };
        let drawn = format!("{:?}", app.results_screen());
        assert!(drawn.contains("Previous page"), "{drawn}");
        assert!(drawn.contains("Next page"));
        assert_eq!(
            drawn.matches("state: Disabled").count(),
            2,
            "one page means neither control leads anywhere: {drawn}"
        );

        let paged = PretNumerique {
            view: View::Results,
            results: (0..=rows() * 2)
                .map(|index| publication(&format!("Title {index}"), vec![source()]))
                .collect(),
            ..PretNumerique::default()
        };
        let first = format!("{:?}", paged.results_screen());
        assert!(first.contains("Page 1 of 3"));
        assert_eq!(first.matches("state: Disabled").count(), 1);

        let mut runner = AppRunner::new(paged);
        runner.start();
        runner.action(action_id(NEXT_PAGE));
        runner.action(action_id(NEXT_PAGE));
        let last = format!("{:?}", runner.app().results_screen());
        assert!(last.contains("Page 3 of 3"));
        assert!(last.contains(&format!("Title {}", rows() * 2)));
        assert_eq!(last.matches("state: Disabled").count(), 1);
        // And it stops there rather than paging into nothing.
        runner.action(action_id(NEXT_PAGE));
        assert_eq!(runner.app().results_page, 2);
    }

    #[test]
    fn an_empty_list_offers_no_pages_at_all() {
        let app = PretNumerique {
            view: View::Results,
            ..PretNumerique::default()
        };
        let drawn = format!("{:?}", app.results_screen());
        assert!(drawn.contains("No titles found."));
        assert!(!drawn.contains("Next page"));
    }

    /// A page of books, with a count the caller chooses so that these tests
    /// keep meaning what they say if the panel's page height changes.
    fn browse_body(page: u32, count: usize, next: bool) -> Vec<u8> {
        let rows = (0..count)
            .map(|index| {
                format!(
                    r#"{{"title":"Title {index}","sources":[{{"handle":"h{index}","catalog":"montreal","catalog_name":"Montreal","is_available":true}}]}}"#
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        format!(
            r#"{{"publications":[{rows}],"page":{page},"has_next":{next},"has_previous":{},"sorts":[{{"key":"issued_on_desc","label":"Recent releases"}}]}}"#,
            page > 1
        )
        .into_bytes()
    }

    #[test]
    fn a_browse_asks_the_server_whether_there_is_another_page() {
        // A page that is exactly full is not evidence of another one, so the
        // answer has to come from the server rather than from the row count.
        let full_and_final = browse_body(4, browse_rows(), false);
        let (publications, sorts, paging) =
            parse_browse(&full_and_final).expect("a readable book list");
        assert_eq!(publications.len(), browse_rows());
        assert_eq!(sorts[0].label, "Recent releases");
        assert_eq!(paging.page, 4);
        assert!(paging.has_previous);
        assert!(!paging.has_next);

        let mut app = PretNumerique {
            view: View::Browse,
            ..PretNumerique::default()
        };
        let mut context = Context::default();
        app.handle_completed(&mut context, super::RequestKind::Browse, &full_and_final);
        let bounds = app.browse_bounds();
        assert!(!bounds.has_next, "a full last page must not offer another");
        assert!(bounds.has_previous);
        let drawn = format!("{:?}", app.browse_screen());
        assert_eq!(drawn.matches("state: Disabled").count(), 1);
        // And a Next tap on the last page asks for nothing.
        app.turn_page(&mut context, true);
        assert!(app.inflight.is_none());

        // The first page of many is the mirror image.
        app.handle_completed(
            &mut context,
            super::RequestKind::Browse,
            &browse_body(1, browse_rows(), true),
        );
        let bounds = app.browse_bounds();
        assert!(bounds.has_next);
        assert!(!bounds.has_previous);
        assert_eq!(
            format!("{:?}", app.browse_screen())
                .matches("state: Disabled")
                .count(),
            1
        );
    }

    #[test]
    fn a_server_page_is_turned_on_the_panel_before_another_is_asked_for() {
        // Two panels' worth and one row over, so the slice on the panel is
        // turned twice before the server is asked for anything.
        let held = browse_rows() * 2 + 1;
        let mut app = PretNumerique {
            view: View::Browse,
            ..PretNumerique::default()
        };
        let mut context = Context::default();
        app.handle_completed(
            &mut context,
            super::RequestKind::Browse,
            &browse_body(1, held, true),
        );
        assert_eq!(app.browse_offset, 0);
        assert!(app.browse_bounds().has_next);

        app.turn_page(&mut context, true);
        assert_eq!(app.browse_offset, 1, "the panel turns first");
        assert!(app.inflight.is_none(), "nothing is asked for yet");
        app.turn_page(&mut context, true);
        assert_eq!(app.browse_offset, 2);
        assert!(app.inflight.is_none());
        // The server's page is used up, so now the next one is asked for.
        app.turn_page(&mut context, true);
        assert_eq!(
            app.inflight.map(|(_, kind)| kind),
            Some(super::RequestKind::Browse)
        );
        assert_eq!(app.browse.page, 2);
        assert_eq!(app.browse_number, 4);

        // Turning back across the boundary lands on the end of the page before,
        // not on its beginning.
        app.inflight = None;
        app.handle_completed(
            &mut context,
            super::RequestKind::Browse,
            &browse_body(2, browse_rows(), false),
        );
        app.turn_page(&mut context, false);
        assert!(app.browse_from_end);
        app.inflight = None;
        app.handle_completed(
            &mut context,
            super::RequestKind::Browse,
            &browse_body(1, held, true),
        );
        assert_eq!(app.browse_offset, 2, "back onto the last panel of page 1");
    }

    #[test]
    fn choosing_an_order_starts_the_list_again_from_its_first_page() {
        let mut app = AppRunner::new(PretNumerique {
            view: View::Browse,
            browse: BrowseQuery {
                title: "Children's".to_owned(),
                category: Some("Y".to_owned()),
                audience: None,
                author: None,
                sort: None,
                page: 3,
                available_only: false,
                bookstore: false,
            },
            browsed: vec![publication("Somewhere in the middle", vec![source()])],
            sorts: vec![
                Sort {
                    key: "created_at_desc".to_owned(),
                    label: "Recent acquisitions".to_owned(),
                },
                Sort {
                    key: "issued_on_desc".to_owned(),
                    label: "Recent releases".to_owned(),
                },
            ],
            browse_offset: 2,
            browse_number: 9,
            ..PretNumerique::default()
        });
        app.start();
        // The orders are on their own screen, reached from the list's bar.
        let list = format!("{:?}", app.app().browse_screen());
        assert!(list.contains("Filters"));
        assert!(!list.contains("Recent acquisitions"));
        app.action(action_id(FILTERS));
        assert_eq!(app.app().view, View::Filters);
        let drawn = format!("{:?}", app.app().filters_screen());
        assert!(drawn.contains("Recent acquisitions"));
        assert!(drawn.contains("Recent releases"));

        let commands = app.action(action_id("sort.1"));
        assert_eq!(app.app().browse.sort.as_deref(), Some("issued_on_desc"));
        assert_eq!(app.app().browse.page, 1);
        assert_eq!(app.app().browse_number, 1);
        assert_eq!(app.app().browse_offset, 0);
        let url = fetched(commands).expect("a new order is a new request");
        assert!(url.contains("sort=issued_on_desc"), "{url}");
        assert!(url.contains("page=1"), "{url}");
        assert!(url.contains("category=Y"), "{url}");
    }

    #[test]
    fn tapping_an_author_lists_their_other_books_across_the_libraries() {
        let mut app = AppRunner::new(PretNumerique {
            view: View::Detail,
            detail: Some(Publication {
                authors: vec!["Marie-Ève Larochelle".to_owned()],
                ..publication("Fixture title", vec![source()])
            }),
            ..PretNumerique::default()
        });
        app.start();
        let drawn = format!("{:?}", app.app().detail_screen());
        assert!(drawn.contains("More by this author"));
        assert!(drawn.contains("Marie-Ève Larochelle"));

        let commands = app.action(action_id("author.0"));
        assert_eq!(app.app().view, View::Browse);
        assert_eq!(app.app().browse.title, "Books by Marie-Ève Larochelle");
        let url = fetched(commands).expect("an author is browsed for");
        assert!(url.starts_with(&format!("{API}/browse?")), "{url}");
        assert!(url.contains("author=Marie-%C3%88ve%20Larochelle"), "{url}");
        // Back from an author's list is the book it was opened from.
        app.action(action_id(BACK));
        assert_eq!(app.app().view, View::Detail);
    }

    #[test]
    fn a_subject_leads_to_a_browse_of_that_subject() {
        let categories = parse_categories(
            br#"{"categories":[{"name":"Children's, Teenage & Educational","key":"Y","catalogs":["montreal","banq"],"total":11594}]}"#,
        )
        .expect("a readable subject list");
        assert_eq!(categories[0].key, "Y");
        assert_eq!(
            category_summary(&categories[0]),
            "11,594 books · Montréal and BAnQ"
        );

        let mut app = AppRunner::new(PretNumerique {
            view: View::Categories,
            categories,
            ..PretNumerique::default()
        });
        app.start();
        assert!(format!("{:?}", app.app().categories_screen()).contains("Browse by subject"));
        let commands = app.action(action_id("category.0"));
        assert_eq!(app.app().view, View::Browse);
        let url = fetched(commands).expect("a subject is browsed for");
        assert!(url.contains("category=Y"), "{url}");
    }

    #[test]
    fn bookstore_categories_and_a_book_category_keep_the_cross_reference_shape() {
        let categories = parse_bookstore_categories(
            br#"{"categories":[{"id":"231","name":"Romans 10-14 ans","path":"Jeunesse / Romans 10-14 ans","held":16,"bookstore_total":25561}]}"#,
        )
        .expect("a readable bookstore category list");
        assert_eq!(categories[0].path, "Jeunesse / Romans 10-14 ans");
        assert_eq!(
            bookstore_category_summary(&categories[0]),
            "16 in your libraries · 25,561 in leslibraires.ca"
        );
        assert_eq!(
            parse_bookstore_category(
                br#"{"isbn":"9782898636363","state":"known","category":{"id":"231","path":"Jeunesse / Romans 10-14 ans"}}"#,
            ),
            Some((
                "known".to_owned(),
                Some("Jeunesse / Romans 10-14 ans".to_owned()),
            ))
        );
    }

    #[test]
    fn a_book_leads_to_its_neighbours_and_they_page() {
        let mut app = AppRunner::new(PretNumerique {
            view: View::Detail,
            detail: Some(publication("Fixture title", vec![source()])),
            ..PretNumerique::default()
        });
        app.start();
        assert!(format!("{:?}", app.app().detail_screen()).contains("Similar"));
        let commands = app.action(action_id(RELATED));
        assert_eq!(app.app().view, View::Related);
        let url = fetched(commands).expect("neighbours are asked for");
        assert_eq!(
            url,
            format!("{API}/publications/Fixture%20title-handle/related")
        );

        let mut app = PretNumerique {
            view: View::Related,
            detail: Some(publication("Fixture title", vec![source()])),
            related: (0..related_rows() + 2)
                .map(|index| publication(&format!("Neighbour {index}"), vec![source()]))
                .collect(),
            ..PretNumerique::default()
        };
        let drawn = format!("{:?}", app.related_screen());
        assert!(drawn.contains("Like Fixture title"));
        assert!(drawn.contains("Page 1 of 2"));
        assert!(drawn.contains(&format!("Neighbour {}", related_rows() - 1)));
        assert!(!drawn.contains(&format!("Neighbour {}", related_rows())));
        let mut context = Context::default();
        app.turn_page(&mut context, true);
        let second = format!("{:?}", app.related_screen());
        assert!(second.contains(&format!("Neighbour {}", related_rows() + 1)));
        assert_eq!(second.matches("state: Disabled").count(), 1);
    }

    #[test]
    fn the_shelf_carries_the_dates_and_the_place_in_the_queue() {
        let shelf = parse_shelf(
            r#"[{"id":"a","title":"Les triplettes - Tome 1","authors":["Ariane Michaud"],"catalog":"montreal","kind":"loan","since":"2026-08-14T19:06:05.328Z","until":"2026-09-11T19:06:05.328Z"},
                 {"id":"b","title":"Mûre secrète","authors":["Sandra Verilli"],"catalog":"montreal","kind":"hold","since":"2026-08-24T01:05:50.211Z","until":"2026-10-24T00:57:57.431Z","position":7,"total":7},
                 {"id":"c","title":"Typiquement Eliza","authors":["Sophie Lee"],"catalog":"banq","kind":"hold","since":"2026-08-24T01:04:22.672Z","until":"2026-12-17T02:26:59.919Z","position":1,"total":4}]"#.as_bytes(),
        )
        .expect("a readable shelf");
        assert_eq!(shelf.len(), 3);
        assert!(shelf[0].is_loan());
        assert!(shelf[1].is_hold());

        let app = PretNumerique {
            view: View::Library,
            books: vec![Book {
                id: "book-id".to_owned(),
                title: "Les Triplettes  -  Tome 1".to_owned(),
                catalog: "montreal".to_owned(),
                file_name: "Les triplettes.lcpl".to_owned(),
                return_state: None,
            }],
            shelf,
            ..PretNumerique::default()
        };
        // The loan's due date comes from the library, matched to the file the
        // home server has.
        assert_eq!(
            app.book_summary(&app.books[0]),
            "Montréal · Due 11 September 2026"
        );
        let library = format!("{:?}", app.library_screen());
        assert!(library.contains("Due 11 September 2026"));
        assert!(library.contains("On hold"));
        assert!(library.contains("2 books · 1 at the front of the queue"));

        let holds = format!("{:?}", app.holds_screen());
        assert!(holds.contains("Montréal · 7th of 7 · 24 August to 24 October 2026"));
        assert!(holds.contains("BAnQ · 1st of 4 · 24 August to 17 December 2026"));
        assert!(holds.contains("only be cancelled from your library account"));
    }

    #[test]
    fn a_loan_the_home_server_does_not_have_is_still_reported() {
        let app = PretNumerique {
            view: View::Library,
            shelf: vec![ShelfEntry {
                title: "Borrowed on the library website".to_owned(),
                authors: Vec::new(),
                catalog: "banq".to_owned(),
                kind: "loan".to_owned(),
                since: None,
                until: Some("2026-09-12".to_owned()),
                position: None,
                total: None,
            }],
            ..PretNumerique::default()
        };
        assert_eq!(app.unheld_loans(), 1);
        let library = format!("{:?}", app.library_screen());
        assert!(library.contains("one loan that is not on your home server"));
    }

    #[test]
    fn dates_are_read_as_dates_and_nothing_else_is_drawn() {
        assert_eq!(
            plain_date("2026-09-11T19:06:05.328Z").as_deref(),
            Some("11 September 2026")
        );
        assert_eq!(plain_date("2026-01-01").as_deref(), Some("1 January 2026"));
        assert_eq!(plain_date("soon"), None);
        assert_eq!(plain_date("2026-13-01"), None);
        assert_eq!(plain_date("2026-09-32"), None);
        assert_eq!(ordinal(1), "1st");
        assert_eq!(ordinal(2), "2nd");
        assert_eq!(ordinal(3), "3rd");
        assert_eq!(ordinal(4), "4th");
        assert_eq!(ordinal(11), "11th");
        assert_eq!(ordinal(21), "21st");
    }

    /// A PDF on this panel is a photographed paper page, so a book neither
    /// library offers any other way is marked and left out until it is asked
    /// for. The server decides which those are: a book with an EPUB is not one
    /// of them because a PDF exists beside it.
    #[test]
    fn a_pdf_only_book_says_so_and_is_kept_out_of_a_list_until_it_is_asked_for() {
        let books = parse_publications(
            r#"{"publications":[
                {"handle":"pdf","title":"Le guide illustré","authors":["Lise Tremblay"],"pdf_only":true,
                 "sources":[{"handle":"m","catalog":"montreal","catalog_name":"Montréal","availability":"Available now","is_available":true}]},
                {"handle":"both","title":"Mensonges","authors":["Marilou Addison"],"formats":["epub","pdf"],
                 "sources":[{"handle":"m2","catalog":"montreal","catalog_name":"Montréal","availability":"Available now","is_available":true}]},
                {"handle":"only","title":"Un autre guide","authors":["Lise Tremblay"],"format":"pdf",
                 "sources":[{"handle":"m3","catalog":"montreal","catalog_name":"Montréal","availability":"Available now","is_available":true}]}]}"#
                .as_bytes(),
        )
        .expect("a readable book list");
        assert!(books[0].pdf_only);
        assert!(!books[1].pdf_only, "an EPUB beside a PDF is not a PDF book");
        assert!(books[2].pdf_only);
        assert_eq!(
            publication_summary(&books[0]),
            "L. Tremblay · Available · PDF"
        );
        assert!(!publication_summary(&books[1]).contains("PDF"));

        let app = PretNumerique {
            view: View::Detail,
            detail: Some(books[0].clone()),
            ..PretNumerique::default()
        };
        assert!(format!("{:?}", app.detail_screen()).contains("PDF only, hard to read here"));
        assert!(!format!(
            "{:?}",
            PretNumerique {
                view: View::Detail,
                detail: Some(books[1].clone()),
                ..PretNumerique::default()
            }
            .detail_screen()
        )
        .contains("PDF"));
    }

    /// The PDF filter is this app's own rather than the catalogue's, so it is
    /// on the filters screen whatever the catalogue sent, and reachable from a
    /// list that was offered no orders at all.
    #[test]
    fn asking_for_pdf_books_is_one_chip_and_starts_off() {
        // A search sets it up before it is run, so the chip is on that screen.
        let mut search = AppRunner::new(PretNumerique {
            view: View::Search,
            query: "potager".to_owned(),
            ..PretNumerique::default()
        });
        search.start();
        let drawn = format!("{:?}", search.app().search_screen());
        assert!(drawn.contains("Show PDF"));
        assert!(!search.app().show_pdf, "hidden until it is asked for");
        let body = posted(search.action(action_id(SUBMIT_SEARCH)))
            .expect("a search should post")
            .1;
        assert!(body.contains("\"include_pdf\":false"), "{body}");

        search.action(action_id(SHOW_PDF));
        assert!(search.app().show_pdf);
        let body = posted(search.action(action_id(SUBMIT_SEARCH)))
            .expect("a search should post")
            .1;
        assert!(body.contains("\"include_pdf\":true"), "{body}");

        // On a list already drawn it changes what is in the list, so the list
        // is asked for again from its first page.
        let mut browse = AppRunner::new(PretNumerique {
            view: View::Browse,
            browse: BrowseQuery {
                title: "Children's".to_owned(),
                category: Some("Y".to_owned()),
                audience: None,
                author: None,
                sort: None,
                page: 4,
                available_only: false,
                bookstore: false,
            },
            browsed: vec![publication("Somewhere in the middle", vec![source()])],
            // A list the catalogue offered no orders for. The filter is still
            // there: it never depended on the catalogue and now cannot.
            sorts: Vec::new(),
            browse_number: 9,
            ..PretNumerique::default()
        });
        browse.start();
        browse.action(action_id(FILTERS));
        let drawn = format!("{:?}", browse.app().filters_screen());
        assert!(drawn.contains("Include PDF"), "{drawn}");
        assert!(
            drawn.contains("This list comes in one order only."),
            "{drawn}"
        );
        let url = fetched(browse.action(action_id(SHOW_PDF))).expect("the list is read again");
        assert!(url.contains("include_pdf=1"), "{url}");
        assert!(url.contains("page=1"), "{url}");
        assert_eq!(browse.app().browse_number, 1);
        // And the list says what was applied without being opened again.
        assert!(
            format!("{:?}", browse.app().browse_screen()).contains("PDF included"),
            "the list must report its own filters"
        );
    }

    #[test]
    fn a_hold_is_offered_only_when_no_library_has_a_copy() {
        let borrowable = PretNumerique {
            view: View::Detail,
            detail: Some(publication("Out at one", vec![out_of_copies(), source()])),
            ..PretNumerique::default()
        };
        let drawn = format!("{:?}", borrowable.detail_screen());
        assert!(drawn.contains("Choose a library"));
        assert!(drawn.contains("Available now at Montréal"));
        assert!(drawn.contains("Borrow & send"));
        assert!(
            !drawn.contains("Place a hold"),
            "a book another library has is not a hold: {drawn}"
        );

        let held = PretNumerique {
            view: View::Detail,
            detail: Some(publication("Out everywhere", vec![out_of_copies()])),
            ..PretNumerique::default()
        };
        let drawn = format!("{:?}", held.detail_screen());
        assert!(drawn.contains("Place a hold"));
        assert!(drawn.contains("Every copy is out at BAnQ"));
        assert!(drawn.contains("puts you on its waiting list"));
        assert!(!drawn.contains("Borrow & send"));
    }

    #[test]
    fn placing_a_hold_is_one_tap_and_carries_no_catalog_link() {
        let mut app = AppRunner::new(PretNumerique {
            view: View::Detail,
            detail: Some(publication("Out everywhere", vec![out_of_copies()])),
            ..PretNumerique::default()
        });
        app.start();
        let commands = app.action(action_id("source.0"));
        let drawn = commands
            .iter()
            .find_map(|command| match command {
                Command::SetScreen(screen) => Some(format!("{screen:?}")),
                _ => None,
            })
            .expect("the tap must draw something");
        assert!(
            drawn.contains("Sending the request"),
            "the progress must be on the same frame as the tap: {drawn}"
        );
        let (url, body) = posted(commands).expect("a hold should post");
        assert_eq!(url, format!("{API}/holds"));
        assert!(body.contains("banq-handle"));
        assert!(body.contains("kobo-hold-"));
        assert!(!body.contains("https://"));
        // The reader stays on the book, exactly as a borrow leaves them there.
        assert_eq!(app.app().view, View::Detail);

        // And a copy that is out while another library has one is not a hold.
        let mut mixed = AppRunner::new(PretNumerique {
            view: View::Detail,
            detail: Some(publication("Out at one", vec![out_of_copies(), source()])),
            ..PretNumerique::default()
        });
        mixed.start();
        let commands = mixed.action(action_id("source.0"));
        assert!(posted(commands).is_none(), "nothing may be sent for it");
        assert!(mixed
            .app()
            .note
            .as_deref()
            .is_some_and(|note| note.contains("Another library has one")));
    }

    #[test]
    fn a_placed_hold_says_what_it_got_and_never_claims_a_loan() {
        let placed = job("hold-job", "hold", "complete", None, None);
        let (level, message) = settled_message(&placed);
        assert_eq!(level, BannerLevel::Info);
        assert_eq!(
            message,
            "You are in line at Montréal. Your holds are in Library."
        );
        assert!(!message.contains("Borrowed"));
        assert_eq!(
            settled_message(&job("hold-job", "hold", "hold_placed", None, None)).1,
            "You are in line at Montréal. Your holds are in Library."
        );
        assert_eq!(kind_label("hold"), "Hold");
        assert!(is_active_state("holding"));
        for state in ["holding", "held", "hold_placed", "hold_uncertain"] {
            assert_ne!(state_label(state), state, "{state} is shown raw");
        }
    }

    #[test]
    fn a_hold_the_library_never_confirmed_needs_a_person() {
        let mut app = AppRunner::new(PretNumerique {
            view: View::Library,
            jobs: vec![job(
                "hold-job",
                "hold",
                "hold_uncertain",
                None,
                Some("Montréal took the request and then stopped answering."),
            )],
            ..PretNumerique::default()
        });
        app.start();
        let library = format!("{:?}", app.app().library_screen());
        assert!(library.contains("Needs your attention"));
        assert!(library.contains("Montréal · Hold · Check your library account"));

        app.action(action_id("resolve.0"));
        assert_eq!(app.app().view, View::Resolve);
        let resolve = format!("{:?}", app.app().resolve_screen());
        assert!(resolve.contains("cannot tell whether Montréal put you in line"));
        assert!(resolve.contains("places no hold and cancels none"));
        assert!(resolve.contains("I checked my account"));
        assert_eq!(
            posted(app.action(action_id(ACKNOWLEDGE)))
                .expect("acknowledge should post")
                .0,
            format!("{API}/jobs/hold-job/acknowledge")
        );
    }

    #[test]
    fn a_hold_can_only_be_given_up_at_the_library() {
        let mut app = AppRunner::new(PretNumerique {
            view: View::Holds,
            shelf: vec![ShelfEntry {
                title: "Mûre secrète".to_owned(),
                authors: vec!["Sandra Verilli".to_owned()],
                catalog: "montreal".to_owned(),
                kind: "hold".to_owned(),
                since: None,
                until: None,
                position: Some(7),
                total: Some(7),
            }],
            ..PretNumerique::default()
        });
        app.start();
        let drawn = format!("{:?}", app.app().holds_screen());
        assert!(
            !drawn.contains("Cancel"),
            "a cancel we cannot make: {drawn}"
        );
        app.action(action_id("hold.0"));
        assert!(app
            .app()
            .note
            .as_deref()
            .is_some_and(|note| note.contains("only be cancelled from your library account")));
    }

    #[test]
    fn the_library_reads_the_loans_the_things_needing_a_person_and_the_shelf() {
        let mut app = PretNumerique::default();
        let mut context = Context::default();
        app.handle_completed(&mut context, super::RequestKind::Books, b"[]");
        assert_eq!(app.queued_request, Some(super::RequestKind::Jobs));
        app.handle_completed(&mut context, super::RequestKind::Jobs, br#"{"jobs":[]}"#);
        assert_eq!(app.queued_request, Some(super::RequestKind::Shelf));
        assert!(app.drain_queued(&mut context), "the shelf read is started");
        assert_eq!(app.queued_request, None);
        assert_eq!(
            app.inflight.map(|(_, kind)| kind),
            Some(super::RequestKind::Shelf)
        );
    }

    #[test]
    fn a_borrow_is_never_left_waiting_behind_a_list_being_read() {
        // The tap is the borrow, and Discover reads the home page on start, so
        // a reader who gets to a book quickly must not be told to wait.
        let mut app = AppRunner::new(PretNumerique {
            view: View::Detail,
            detail: Some(publication("Fixture title", vec![source()])),
            ..PretNumerique::default()
        });
        app.start();
        assert!(app.app().inflight.is_some(), "the home page is being read");
        let commands = app.action(action_id("source.0"));
        assert_eq!(
            posted(commands).map(|(url, _)| url),
            Some(format!("{API}/jobs")),
            "a read must be dropped rather than block the borrow"
        );
    }

    #[test]
    fn taking_the_way_out_hands_the_panel_back() {
        let mut runner = AppRunner::new(populated());
        runner.start();
        let commands = runner.action(action_id(READER));
        assert!(
            commands
                .iter()
                .any(|command| matches!(command, Command::Exit)),
            "the reader slot did not end the session: {commands:?}"
        );
        // And says so first, because the stock reader takes half a minute.
        let drawn = commands.iter().find_map(|command| match command {
            Command::SetScreen(screen) => Some(format!("{screen:?}")),
            _ => None,
        });
        let drawn = drawn.expect("a screen explaining the wait");
        assert!(drawn.contains("Returning to the Kobo reader"), "{drawn}");
    }
}
