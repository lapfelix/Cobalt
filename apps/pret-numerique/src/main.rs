//! A Kobo-only client for the server-side Prêt numérique proxy.
//!
//! The reader holds only opaque publication handles and the last search it was
//! given. It never receives an OPDS URL, an LCPL, an EPUB, or either library's
//! login material, and it keeps no record of a request between launches: the
//! server owns that list and is asked for it. Every request that changes
//! something on the server uses `spawn`, while searches and status reads use
//! `spawn_retrying`.
//!
//! There is no screen listing jobs. A borrow or a return is followed where the
//! reader started it, a success lands in Library because that is where they
//! would look, and the outcomes only a person can settle appear against the
//! loan they belong to -- or, when there is no loan to attach them to, in a
//! section of Library that exists only while something is unresolved.

use kobo_json::{ObjectBuilder, Value};
use kobo_sdk::keyboard::{TextEntry, Typing};
use kobo_sdk::{
    action_id, BannerLevel, Context, Credential, Failure, Glyph, Header, KoboApp, Screen,
    ScreenBuilder, Space, StoreResult, Task, TaskError, TaskId, TaskOutcome,
};
use std::process::ExitCode;

const API: &str = "https://home.lapal.me:3300/pret/v1";
const API_SECRET: &str = "pret-numerique-api";
const STORE_STATE: &str = "ui-state";
const MAX_RESULTS: usize = 40;
const MAX_QUERY_CHARS: usize = 80;
const MAX_DETAIL_DESCRIPTION_CHARS: usize = 240;
const MAX_ERROR_CHARS: usize = 160;
const MAX_JOBS: usize = 24;
const POLL_SECONDS: u32 = 2;
const SEARCH: &str = "search-tab";
const LIBRARY: &str = "library-tab";
const SETTINGS: &str = "settings-tab";
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
    #[default]
    Search,
    Results,
    Detail,
    Library,
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

#[derive(Clone, Debug, PartialEq)]
struct SearchResult {
    title: String,
    authors: Vec<String>,
    isbn: Option<String>,
    description: Option<String>,
    goodreads_rating: Option<f64>,
    goodreads_ratings_count: Option<i64>,
    goodreads_reviews_count: Option<i64>,
    sources: Vec<Source>,
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
    Books,
    Jobs,
    Job,
    Health,
    Borrow,
    Return,
    RetryHook,
    Acknowledge,
}

struct PretNumerique {
    view: View,
    filter: CatalogFilter,
    query: String,
    entry: TextEntry,
    results: Vec<SearchResult>,
    selected_result: Option<usize>,
    selected_source: usize,
    books: Vec<Book>,
    selected_book: Option<usize>,
    jobs: Vec<Job>,
    resolve_job: Option<String>,
    watch: Option<Watch>,
    inflight: Option<(TaskId, RequestKind)>,
    queued_request: Option<RequestKind>,
    sleep_task: Option<TaskId>,
    loading: bool,
    note: Option<String>,
    health: Option<String>,
    health_advice: Option<String>,
    loaded_state: bool,
}

impl Default for PretNumerique {
    fn default() -> Self {
        Self {
            view: View::Search,
            filter: CatalogFilter::All,
            query: String::new(),
            entry: TextEntry::new().opened_by(EDIT_QUERY),
            results: Vec::new(),
            selected_result: None,
            selected_source: 0,
            books: Vec::new(),
            selected_book: None,
            jobs: Vec::new(),
            resolve_job: None,
            watch: None,
            inflight: None,
            queued_request: None,
            sleep_task: None,
            loading: false,
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
    }

    fn save_state(&mut self, context: &mut Context) {
        context.store().save(STORE_STATE, self.state_bytes());
    }

    fn show(&self, context: &mut Context) {
        context.set_screen(self.screen());
    }

    fn screen(&self) -> Screen {
        match self.view {
            View::Search => self.search_screen(),
            View::Results => self.results_screen(),
            View::Detail => self.detail_screen(),
            View::Library => self.library_screen(),
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
    fn nav(screen: ScreenBuilder, selected: usize) -> ScreenBuilder {
        screen.nav_bar(
            Some(selected),
            [
                (SEARCH, "Search"),
                (LIBRARY, "Library"),
                (SETTINGS, "Settings"),
                (READER, "Kobo reader"),
            ],
        )
    }

    fn leaving_screen() -> Screen {
        ScreenBuilder::new("leaving")
            .top_bar("Returning")
            .heading("Returning to the Kobo reader")
            .text("The reader takes about half a minute to start and rescan.")
            .build()
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
        let mut screen = Self::nav(ScreenBuilder::new("search").top_bar("Prêt numérique"), 0);
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
            ])
            .button(SUBMIT_SEARCH, "Search")
            .spacer(Space::Small);
        if let Some(note) = &self.note {
            screen = screen.banner(BannerLevel::Info, note.clone());
        }
        screen.build()
    }

    fn results_screen(&self) -> Screen {
        let mut screen = Self::nav(ScreenBuilder::new("results").top_bar("Results"), 0);
        screen = screen
            .heading(self.filter.label())
            .text(format!("Search: {}", compact_message(&self.query, 56)))
            .top_bar_action(REFRESH, "Refresh");
        if self.loading {
            return screen.skeleton(6).build();
        }
        if self.results.is_empty() {
            screen = screen.empty_state("No titles found.");
        } else {
            screen = screen.rows(self.results.iter().enumerate().map(|(index, result)| {
                let catalogs = result
                    .sources
                    .iter()
                    .map(|source| source.catalog_name.as_str())
                    .collect::<Vec<_>>()
                    .join(" · ");
                let availability = if result.sources.iter().any(|source| source.available) {
                    "Available"
                } else {
                    "Not currently available"
                };
                let goodreads = result
                    .goodreads_rating
                    .map(|rating| format!(" · Goodreads {rating:.1}/5"))
                    .unwrap_or_default();
                (
                    format!("result.{index}"),
                    result.title.clone(),
                    format!(
                        "{} · {catalogs} · {availability}{goodreads}",
                        author_line(&result.authors),
                    ),
                    Glyph::Book,
                )
            }));
        }
        if let Some(note) = &self.note {
            screen = screen.banner(BannerLevel::Attention, note.clone());
        }
        screen.build()
    }

    fn detail_screen(&self) -> Screen {
        let Some(index) = self.selected_result else {
            return self.results_screen();
        };
        let Some(result) = self.results.get(index) else {
            return self.results_screen();
        };
        let mut screen = Self::nav(
            ScreenBuilder::new("detail")
                .top_bar("Book details")
                .top_bar_action(BACK, "Back"),
            0,
        )
        .heading(result.title.clone());
        screen = self.watch_block(screen).facts([
            ("Author", author_line(&result.authors)),
            (
                "ISBN",
                result
                    .isbn
                    .clone()
                    .unwrap_or_else(|| "Not listed".to_owned()),
            ),
        ]);
        if let Some(description) = &result.description {
            screen = screen
                .section("About this book")
                .text(compact_message(description, MAX_DETAIL_DESCRIPTION_CHARS));
        }
        if let Some(rating) = result.goodreads_rating {
            let ratings = result.goodreads_ratings_count.map_or_else(
                || "ratings not listed".to_owned(),
                |count| format!("{} ratings", count_label(count)),
            );
            let reviews = result.goodreads_reviews_count.map_or_else(
                || "reviews not listed".to_owned(),
                |count| format!("{} reviews", count_label(count)),
            );
            screen = screen
                .section("Goodreads")
                .secondary(format!("{rating:.1} / 5 · {ratings} · {reviews}"));
        }
        // The tap is the borrow, so the row has to say so before it is taken.
        screen = screen
            .section("Choose a library")
            .secondary("Tapping one borrows it. The book is saved on your home server.");
        screen = screen.rows(
            result
                .sources
                .iter()
                .enumerate()
                .map(|(source_index, source)| {
                    let status = availability_label(source);
                    let action = if source.available {
                        "Borrow & send"
                    } else {
                        "Unavailable"
                    };
                    (
                        format!("source.{source_index}"),
                        source.catalog_name.clone(),
                        format!("{status} · {action}"),
                        Glyph::Globe,
                    )
                }),
        );
        if let Some(note) = &self.note {
            screen = screen.banner(BannerLevel::Attention, note.clone());
        }
        screen.build()
    }

    fn library_screen(&self) -> Screen {
        let mut screen = Self::nav(ScreenBuilder::new("library").top_bar("My loans"), 1)
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
        if self.loading && self.books.is_empty() {
            return screen.skeleton(5).build();
        }
        screen = self.watch_block(screen);
        let unresolved = self.unresolved_jobs();
        if !unresolved.is_empty() {
            screen = screen.section_rows(
                "Needs your attention",
                None,
                unresolved.iter().filter_map(|&index| {
                    let job = self.jobs.get(index)?;
                    Some((
                        format!("resolve.{index}"),
                        job.title.clone(),
                        unresolved_summary(job),
                        Glyph::Circle,
                    ))
                }),
            );
        }
        let books = self.filtered_books();
        if books.is_empty() {
            screen = screen.empty_state("No saved loans for this library.");
        } else {
            screen = screen.rows(books.iter().enumerate().map(|(index, book)| {
                (
                    format!("book.{index}"),
                    book.title.clone(),
                    self.book_summary(book),
                    Glyph::Bookmark,
                )
            }));
        }
        if let Some(note) = &self.note {
            screen = screen.banner(BannerLevel::Attention, note.clone());
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
        if let Some(error) = &job.error {
            screen = screen
                .section("What the home server said")
                .secondary(compact_message(error, MAX_ERROR_CHARS));
        }
        screen = if self.loading {
            screen.activity("Telling the home server...", None)
        } else if job.state == "hook_failed" {
            screen.buttons([(RETRY_HOOK, "Send to my reader again"), (CANCEL, "Not now")])
        } else {
            screen.buttons([(ACKNOWLEDGE, "I checked my account"), (CANCEL, "Not now")])
        };
        if let Some(note) = &self.note {
            screen = screen.banner(BannerLevel::Attention, note.clone());
        }
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
        screen = screen.button(REFRESH, "Check connection");
        if let Some(note) = &self.note {
            screen = screen.banner(BannerLevel::Info, note.clone());
        }
        screen.build()
    }

    fn filtered_books(&self) -> Vec<Book> {
        self.books
            .iter()
            .filter(|book| self.filter.matches(&book.catalog))
            .cloned()
            .collect()
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
                "borrow_uncertain" | "failed" => true,
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
        match book.return_state.as_deref() {
            Some(state) => format!("{catalog} · {}", state_label(state)),
            None => format!("{catalog} · Ready to return"),
        }
    }

    fn start_search(&mut self, context: &mut Context) {
        if self.inflight.is_some() {
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
            .build()
            .to_json();
        self.loading = true;
        self.note = None;
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
            self.loading = false;
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
        self.loading = true;
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
            self.loading = false;
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
        self.loading = true;
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
            self.loading = false;
            self.note = Some("The Kobo runtime is busy. Try again in a moment.".to_owned());
        }
        self.show(context);
    }

    fn start_health(&mut self, context: &mut Context) {
        if self.inflight.is_some() {
            self.show(context);
            return;
        }
        self.loading = true;
        let task = Task::Fetch {
            url: format!("{API}/health"),
            offset: 0,
            max_bytes: 32 * 1024,
            credential: Some(Self::credential()),
            headers: vec![Header::new("Accept", "application/json")],
        };
        if let Some(task_id) = context.spawn_retrying(task) {
            self.inflight = Some((task_id, RequestKind::Health));
        } else {
            self.loading = false;
        }
        self.show(context);
    }

    fn start_borrow(&mut self, context: &mut Context) {
        if self.inflight.is_some() {
            // The tap is the borrow now, so a busy runtime has to answer it
            // rather than swallow it.
            self.note = Some("Still finishing the last request. Try again in a moment.".to_owned());
            self.show(context);
            return;
        }
        let Some(result_index) = self.selected_result else {
            return;
        };
        let Some(result) = self.results.get(result_index) else {
            return;
        };
        let Some(source) = result.sources.get(self.selected_source) else {
            return;
        };
        let body = ObjectBuilder::new()
            .set("publication_handle", source.handle.clone())
            .set(
                "client_request_id",
                format!("kobo-borrow-{}", unique_request_suffix()),
            )
            .build()
            .to_json();
        let task = Task::Post {
            url: format!("{API}/jobs"),
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
            self.inflight = Some((task_id, RequestKind::Borrow));
        } else {
            self.watch = None;
            self.note = Some("The Kobo runtime is busy. The borrow was not sent.".to_owned());
        }
        self.show(context);
    }

    fn start_return(&mut self, context: &mut Context) {
        if self.inflight.is_some() {
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
        if self.inflight.is_some() {
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
        self.loading = true;
        self.note = None;
        if let Some(task_id) = context.spawn(task) {
            self.inflight = Some((task_id, RequestKind::RetryHook));
        } else {
            self.loading = false;
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
        if self.inflight.is_some() {
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
        self.loading = true;
        self.note = None;
        if let Some(task_id) = context.spawn(task) {
            self.inflight = Some((task_id, RequestKind::Acknowledge));
        } else {
            self.loading = false;
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
            _ => false,
        }
    }

    #[allow(clippy::single_match_else, clippy::too_many_lines)]
    fn handle_completed(&mut self, context: &mut Context, kind: RequestKind, body: &[u8]) {
        match kind {
            RequestKind::Search => match parse_search(body) {
                Some(results) => {
                    self.results = results.into_iter().take(MAX_RESULTS).collect();
                    self.loading = false;
                    self.note = parse_catalog_status_note(body);
                }
                None => {
                    self.loading = false;
                    self.note =
                        Some("The proxy returned an unreadable search response.".to_owned());
                }
            },
            // Neither of these clears the note. They are also the reads that
            // follow an acknowledgement, and wiping the confirmation of a
            // decision the reader just made would leave nothing on screen to
            // say it landed. A note is cleared where one is asked for: a
            // refresh, or a move to another screen.
            RequestKind::Books => match parse_books(body) {
                Some(books) => {
                    self.books = books;
                    self.loading = false;
                    // The loans and the things needing a person are one screen,
                    // so they are always read as a pair.
                    self.queued_request = Some(RequestKind::Jobs);
                }
                None => {
                    self.loading = false;
                    self.note =
                        Some("The proxy returned an unreadable library response.".to_owned());
                }
            },
            RequestKind::Jobs => match parse_jobs(body) {
                Some(jobs) => {
                    self.jobs = jobs;
                    self.loading = false;
                }
                None => {
                    self.loading = false;
                    self.note = Some("The proxy returned an unreadable request list.".to_owned());
                }
            },
            RequestKind::Job => match parse_job(body) {
                Some(job) => {
                    self.loading = false;
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
                    self.loading = false;
                    self.note =
                        Some("The proxy returned an unreadable request response.".to_owned());
                }
            },
            RequestKind::Health => match parse_health(body) {
                Some((summary, signed_out)) => {
                    self.health = Some(summary);
                    self.health_advice = sign_in_advice(&signed_out);
                    self.loading = false;
                    self.note = None;
                }
                None => {
                    self.loading = false;
                    self.note =
                        Some("The proxy returned an unreadable health response.".to_owned());
                }
            },
            RequestKind::Borrow | RequestKind::Return => match parse_job(body) {
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
                    self.note = Some("The proxy returned an unreadable job response.".to_owned());
                }
            },
            RequestKind::RetryHook => match parse_job(body) {
                Some(job) => {
                    self.loading = false;
                    self.note = None;
                    self.resolve_job = None;
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
                    self.loading = false;
                    self.note = Some("The proxy returned an unreadable job response.".to_owned());
                }
            },
            RequestKind::Acknowledge => match parse_job(body) {
                Some(job) => {
                    self.loading = false;
                    self.jobs.retain(|existing| existing.id != job.id);
                    self.resolve_job = None;
                    self.view = View::Library;
                    self.note =
                        Some("Cleared. You can try this again whenever you like.".to_owned());
                    self.queued_request = Some(RequestKind::Books);
                }
                None => {
                    self.loading = false;
                    self.note = Some("The proxy returned an unreadable job response.".to_owned());
                }
            },
        }
    }

    fn handle_failed(&mut self, kind: RequestKind, error: TaskError) {
        self.loading = false;
        let advice = Failure::of(error).advice;
        if matches!(
            kind,
            RequestKind::Borrow | RequestKind::Return | RequestKind::RetryHook
        ) {
            self.watch = None;
        }
        // A 409 and a 404 reach an application as the same refusal, so a
        // rejected borrow or return points at the one screen that can tell the
        // reader which it was and offer the way out.
        if matches!(kind, RequestKind::Borrow | RequestKind::Return) && error == TaskError::NotFound
        {
            self.queued_request = Some(RequestKind::Books);
        }
        self.note = Some(match kind {
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

    fn back(&mut self, context: &mut Context) {
        let destination = match self.view {
            View::Search | View::Results | View::Library | View::Settings | View::Leaving => {
                View::Search
            }
            View::Detail => View::Results,
            View::ConfirmReturn | View::Resolve => View::Library,
        };
        self.leave_watch(context, destination);
        self.view = destination;
        self.note = None;
        self.show(context);
    }

    fn go(&mut self, context: &mut Context, destination: View) {
        self.leave_watch(context, destination);
        self.view = destination;
        self.note = None;
        match destination {
            View::Library => self.start_books(context),
            View::Settings => self.start_health(context),
            _ => self.show(context),
        }
    }
}

impl KoboApp for PretNumerique {
    fn on_start(&mut self, context: &mut Context) {
        context.store().load(STORE_STATE);
        self.show(context);
    }

    fn on_store(&mut self, _context: &mut Context, result: StoreResult) {
        if let StoreResult::Loaded { key, value } = result {
            if key == STORE_STATE {
                self.load_state(value.as_deref());
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
        if action == action_id(SEARCH) {
            self.go(context, View::Search);
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
        if action == action_id(FILTER_ALL) {
            self.filter = CatalogFilter::All;
            self.show(context);
            return;
        }
        if action == action_id(FILTER_MONTREAL) {
            self.filter = CatalogFilter::Montreal;
            self.show(context);
            return;
        }
        if action == action_id(FILTER_BANQ) {
            self.filter = CatalogFilter::Banq;
            self.show(context);
            return;
        }
        if action == action_id(SUBMIT_SEARCH) || action == action_id(REFRESH) {
            self.note = None;
            match self.view {
                View::Search | View::Results => self.start_search(context),
                View::Library => {
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
            self.view = View::Resolve;
            self.note = None;
            self.show(context);
            return;
        }
        if let Some(index) = action_index(action, "result", self.results.len()) {
            self.selected_result = Some(index);
            self.selected_source = 0;
            self.view = View::Detail;
            self.note = None;
            self.show(context);
        }
        if let Some(index) = self.selected_result.and_then(|result_index| {
            self.results
                .get(result_index)
                .and_then(|result| action_index(action, "source", result.sources.len()))
        }) {
            self.selected_source = index;
            if self
                .selected_result
                .and_then(|result_index| self.results.get(result_index))
                .and_then(|result| result.sources.get(index))
                .is_some_and(|source| source.available)
            {
                self.start_borrow(context);
            } else {
                self.note = Some("That copy is not currently available.".to_owned());
                self.show(context);
            }
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
            TaskOutcome::Failed(error) => self.handle_failed(kind, error),
            TaskOutcome::Cancelled => {
                self.loading = false;
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
            self.view = View::Search;
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
            self.view = View::Resolve;
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
        self.view = View::ConfirmReturn;
        self.show(context);
    }
}

fn action_index(action: kobo_sdk::ActionId, prefix: &str, count: usize) -> Option<usize> {
    (0..count).find(|index| action == action_id(&format!("{prefix}.{index}")))
}

fn author_line(authors: &[String]) -> String {
    if authors.is_empty() {
        "Unknown author".to_owned()
    } else {
        authors.join(", ")
    }
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
        "borrow_uncertain" | "return_uncertain" => {
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
        "complete" => (
            BannerLevel::Info,
            "Borrowed. It is in your Library now.".to_owned(),
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
        "returning" => "Returning...",
        "returned" => "Returned",
        "return_failed" => "Return failed · loan kept",
        "return_uncertain" => "Return not confirmed",
        "failed" => "Could not finish",
        _ => state,
    }
}

fn is_active_state(state: &str) -> bool {
    matches!(
        state,
        "queued" | "borrowing" | "downloading" | "stored" | "hook_running" | "returning"
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

fn parse_search(body: &[u8]) -> Option<Vec<SearchResult>> {
    let text = std::str::from_utf8(body).ok()?;
    let root = kobo_json::parse(text).ok()?;
    let results = root.get("results")?.as_array()?;
    Some(
        results
            .iter()
            .filter_map(|result| {
                let title = result.get("title")?.as_str()?.to_owned();
                let authors = result
                    .get("authors")
                    .and_then(Value::as_array)
                    .map(|authors| {
                        authors
                            .iter()
                            .filter_map(Value::as_str)
                            .take(5)
                            .map(str::to_owned)
                            .collect()
                    })
                    .unwrap_or_default();
                let sources: Vec<Source> = result
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
                    .unwrap_or_default();
                (!sources.is_empty()).then_some(SearchResult {
                    title,
                    authors,
                    isbn: result
                        .get("isbn")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    description: result
                        .get("description")
                        .and_then(Value::as_str)
                        .map(|value| compact_message(value, 720)),
                    goodreads_rating: result.get("goodreads_rating").and_then(Value::as_f64),
                    goodreads_ratings_count: result
                        .get("goodreads_ratings_count")
                        .and_then(Value::as_i64),
                    goodreads_reviews_count: result
                        .get("goodreads_reviews_count")
                        .and_then(Value::as_i64),
                    sources,
                })
            })
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
        availability_label, compact_message, is_active_state, kind_label, parse_books,
        parse_catalog_status_note, parse_health, parse_job, parse_jobs, parse_search,
        resolve_explanation, settled_message, sign_in_advice, state_label, unresolved_summary,
        Book, Job, PretNumerique, SearchResult, Source, View, Watch, ACKNOWLEDGE, API,
        CONFIRM_RETURN, READER, RETRY_HOOK,
    };
    use kobo_sdk::{
        action_id, AppRunner, BannerLevel, Command, Context, KoboApp, StoreResult, Task, TaskError,
    };
    use kobo_ui::{
        Chrome, DisplayMetrics, LayoutIssueKind, LayoutKind, TextScale, CLARA_BW_METRICS,
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
                results: vec![result],
                selected_result: Some(0),
                ..PretNumerique::default()
            }
            .detail_screen()
        );
        assert!(screen.contains("A bounded description."));
        assert!(screen.contains("4.3 / 5"));
        assert!(screen.contains("1,700,633"));
        assert!(screen.contains("88,668"));
    }

    #[test]
    fn detail_screen_bounds_long_descriptions_before_library_actions() {
        let result = SearchResult {
            title: "Book".to_owned(),
            authors: vec!["Author".to_owned()],
            isbn: None,
            description: Some("A very long description. ".repeat(40)),
            goodreads_rating: None,
            goodreads_ratings_count: None,
            goodreads_reviews_count: None,
            sources: vec![source()],
        };
        let app = PretNumerique {
            results: vec![result],
            selected_result: Some(0),
            ..PretNumerique::default()
        };
        let screen = format!("{:?}", app.detail_screen());
        assert!(screen.contains("Choose a library"));
        assert!(screen.contains("..."));
        assert!(!screen.contains(&"A very long description. ".repeat(40)));
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
        assert!(nav.contains("Search"));
        assert!(nav.contains("Library"));
        assert!(nav.contains("Settings"));
    }

    #[test]
    fn borrow_and_return_actions_only_post_to_the_proxy() {
        let result = SearchResult {
            title: "Fixture title".to_owned(),
            authors: vec!["Fixture author".to_owned()],
            isbn: Some("9780000000000".to_owned()),
            description: None,
            goodreads_rating: None,
            goodreads_ratings_count: None,
            goodreads_reviews_count: None,
            sources: vec![source()],
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
            results: vec![SearchResult {
                title: "Fixture title".to_owned(),
                authors: Vec::new(),
                isbn: None,
                description: None,
                goodreads_rating: None,
                goodreads_ratings_count: None,
                goodreads_reviews_count: None,
                sources: vec![source()],
            }],
            selected_result: Some(0),
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
        app.handle_failed(super::RequestKind::Borrow, TaskError::Offline);
        let offline = app.note.clone().expect("a note");
        assert!(offline.contains("not on a network"));
        assert!(offline.contains("nothing was borrowed"));

        app.handle_failed(super::RequestKind::Borrow, TaskError::NotFound);
        assert!(app
            .note
            .as_deref()
            .is_some_and(|note| note.contains("Look in Library")));
        assert_eq!(app.queued_request, Some(super::RequestKind::Books));

        app.handle_failed(super::RequestKind::Return, TaskError::Offline);
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
            results: vec![SearchResult {
                title: "Fixture title".to_owned(),
                authors: Vec::new(),
                isbn: None,
                description: None,
                goodreads_rating: None,
                goodreads_ratings_count: None,
                goodreads_reviews_count: None,
                sources: vec![source()],
            }],
            selected_result: Some(0),
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
    fn state_store_keeps_the_query_and_nothing_else() {
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
        assert_eq!(app.state_bytes(), b"Dune\n");
    }

    /// Every screen this app can be looked at, except the one it draws on its
    /// way out and the keyboard, which carries its own Cancel.
    const VIEWS: [View; 7] = [
        View::Search,
        View::Results,
        View::Detail,
        View::Library,
        View::ConfirmReturn,
        View::Resolve,
        View::Settings,
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

    fn populated() -> PretNumerique {
        PretNumerique {
            results: vec![SearchResult {
                title: "Fixture title".to_owned(),
                authors: vec!["Fixture author".to_owned()],
                isbn: Some("9780000000000".to_owned()),
                description: None,
                goodreads_rating: None,
                goodreads_ratings_count: None,
                goodreads_reviews_count: None,
                sources: vec![source()],
            }],
            selected_result: Some(0),
            books: vec![book("book-id", "montreal", Some("return_failed"))],
            selected_book: Some(0),
            jobs: vec![job("job-id", "borrow", "borrow_uncertain", None, None)],
            resolve_job: Some("job-id".to_owned()),
            ..PretNumerique::default()
        }
    }

    #[test]
    fn the_way_back_to_the_reader_is_on_every_screen_of_every_panel() {
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
                let layout = screen.layout_with(&metrics, &Chrome::with_back(false));
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
                        .diagnostics(&metrics, &Chrome::with_back(false))
                        .issues
                        .iter()
                        .any(|issue| issue.kind == LayoutIssueKind::TextOverflow
                            && issue.node == Some(slot.id)),
                    "{name}: the way out does not fit its slot on {view:?}"
                );
            }
        }
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
