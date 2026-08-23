//! A Kobo-only client for the server-side Prêt numérique proxy.
//!
//! The reader holds only opaque publication handles, job IDs and UI state. It
//! never receives an OPDS URL, an LCPL, an EPUB, or either library's login
//! material. Borrow and return requests deliberately use `spawn`, while
//! searches and status reads use `spawn_retrying`.

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
const MAX_JOBS: usize = 24;
const SEARCH: &str = "search-tab";
const LIBRARY: &str = "library-tab";
const QUEUE: &str = "queue-tab";
const SETTINGS: &str = "settings-tab";
const EDIT_QUERY: &str = "edit-query";
const SUBMIT_SEARCH: &str = "submit-search";
const FILTER_ALL: &str = "filter-all";
const FILTER_MONTREAL: &str = "filter-montreal";
const FILTER_BANQ: &str = "filter-banq";
const REFRESH: &str = "refresh";
const CONFIRM_BORROW: &str = "confirm-borrow";
const CONFIRM_RETURN: &str = "confirm-return";
const RETRY_HOOK: &str = "retry-hook";
const CANCEL: &str = "cancel-action";
const BACK: &str = "back";

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum View {
    #[default]
    Search,
    Results,
    Detail,
    ConfirmBorrow,
    Library,
    ConfirmReturn,
    Queue,
    Settings,
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

#[derive(Clone, Debug, Eq, PartialEq)]
struct Job {
    id: String,
    kind: String,
    state: String,
    title: String,
    catalog: String,
    error: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RequestKind {
    Search,
    Books,
    Jobs,
    Health,
    Borrow,
    Return,
    RetryHook,
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
    job_ids: Vec<String>,
    inflight: Option<(TaskId, RequestKind)>,
    sleep_task: Option<TaskId>,
    loading: bool,
    note: Option<String>,
    health: Option<String>,
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
            job_ids: Vec::new(),
            inflight: None,
            sleep_task: None,
            loading: false,
            note: None,
            health: None,
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
        for id in self.job_ids.iter().take(MAX_JOBS) {
            bytes.extend_from_slice(id.as_bytes());
            bytes.push(b'\n');
        }
        bytes
    }

    fn load_state(&mut self, value: Option<&[u8]>) {
        let Some(value) = value else {
            self.loaded_state = true;
            return;
        };
        let Ok(text) = std::str::from_utf8(value) else {
            self.loaded_state = true;
            return;
        };
        let mut lines = text.lines();
        lines.next().unwrap_or_default().clone_into(&mut self.query);
        self.job_ids = lines
            .filter(|line| !line.trim().is_empty())
            .take(MAX_JOBS)
            .map(str::to_owned)
            .collect();
        self.loaded_state = true;
    }

    fn save_state(&mut self, context: &mut Context) {
        context.store().save(STORE_STATE, self.state_bytes());
    }

    fn show(&self, context: &mut Context) {
        let screen = match self.view {
            View::Search => self.search_screen(),
            View::Results => self.results_screen(),
            View::Detail => self.detail_screen(),
            View::ConfirmBorrow => self.confirm_borrow_screen(),
            View::Library => self.library_screen(),
            View::ConfirmReturn => self.confirm_return_screen(),
            View::Queue => self.queue_screen(),
            View::Settings => self.settings_screen(),
        };
        context.set_screen(screen);
    }

    fn nav(screen: ScreenBuilder, selected: usize) -> ScreenBuilder {
        screen.nav_bar(
            Some(selected),
            [
                (SEARCH, "Search"),
                (LIBRARY, "Library"),
                (QUEUE, "Queue"),
                (SETTINGS, "Settings"),
            ],
        )
    }

    fn search_screen(&self) -> Screen {
        if self.entry.is_open() {
            return ScreenBuilder::new("search-input")
                .top_bar("Search")
                .text_entry(&self.entry, "Search Montréal and BAnQ", "Search")
                .build();
        }
        let mut screen = Self::nav(ScreenBuilder::new("search").top_bar("Prêt numérique"), 0);
        screen = screen
            .heading("Find a book")
            .text("Search both library catalogues. The proxy handles the loan and keeps the LCPL at home.")
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
            .heading(format!("{} · {}", self.filter.label(), self.query))
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
                let isbn = result.isbn.as_deref().unwrap_or("ISBN not listed");
                let goodreads = result
                    .goodreads_rating
                    .map(|rating| format!(" · Goodreads {rating:.1}/5"))
                    .unwrap_or_default();
                (
                    format!("result.{index}"),
                    result.title.clone(),
                    format!(
                        "{} · ISBN: {isbn} · {catalogs} · {availability}{goodreads}",
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
        let mut screen = Self::nav(ScreenBuilder::new("detail").top_bar("Book details"), 0)
            .heading(result.title.clone())
            .facts([
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
            screen = screen.section("About this book").text(description.clone());
        }
        if let Some(rating) = result.goodreads_rating {
            screen = screen.section("Goodreads").facts([
                ("Score", format!("{rating:.1} / 5")),
                (
                    "Ratings",
                    result
                        .goodreads_ratings_count
                        .map_or_else(|| "Not listed".to_owned(), count_label),
                ),
                (
                    "Reviews",
                    result
                        .goodreads_reviews_count
                        .map_or_else(|| "Not listed".to_owned(), count_label),
                ),
            ]);
        }
        screen = screen.section("Choose a library");
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

    fn confirm_borrow_screen(&self) -> Screen {
        let Some(result_index) = self.selected_result else {
            return self.results_screen();
        };
        let Some(result) = self.results.get(result_index) else {
            return self.results_screen();
        };
        let source = result.sources.get(self.selected_source);
        let library = source.map_or("selected library", |source| source.catalog_name.as_str());
        Self::nav(
            ScreenBuilder::new("confirm-borrow").top_bar("Confirm borrow"),
            0,
        )
        .heading("Borrow and send?")
        .text(format!(
            "{}\n{}\n\nThe LCPL stays on the home server. Nothing is downloaded to this Kobo.",
            result.title, library
        ))
        .buttons([(CONFIRM_BORROW, "Borrow & send"), (CANCEL, "Cancel")])
        .build()
    }

    fn library_screen(&self) -> Screen {
        let mut screen = Self::nav(ScreenBuilder::new("library").top_bar("My loans"), 1)
            .chips([
                (FILTER_ALL, "All", self.filter == CatalogFilter::All),
                (
                    FILTER_MONTREAL,
                    "Montréal",
                    self.filter == CatalogFilter::Montreal,
                ),
                (FILTER_BANQ, "BAnQ", self.filter == CatalogFilter::Banq),
            ])
            .top_bar_action(REFRESH, "Refresh");
        let books = self.filtered_books();
        if self.loading {
            return screen.skeleton(5).build();
        }
        if books.is_empty() {
            screen = screen.empty_state("No saved loans for this library.");
        } else {
            screen = screen.rows(books.iter().enumerate().map(|(index, book)| {
                let state = book
                    .return_state
                    .as_deref()
                    .map_or("Ready to return", state_label);
                (
                    format!("book.{index}"),
                    book.title.clone(),
                    format!("{} · {state}", catalog_label(&book.catalog)),
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
            ScreenBuilder::new("confirm-return").top_bar("Return loan"),
            1,
        )
        .heading("Return this loan?")
        .text(format!(
            "{}\n{}\n\nThe proxy will call the {} return service once. The saved LCPL is removed only after the library confirms the return.",
            book.title,
            book.file_name,
            catalog_label(&book.catalog)
        ))
        .buttons([(CONFIRM_RETURN, "Return loan"), (CANCEL, "Keep loan")])
        .build()
    }

    fn queue_screen(&self) -> Screen {
        let mut screen = Self::nav(ScreenBuilder::new("queue").top_bar("Queue"), 2)
            .top_bar_action(REFRESH, "Refresh")
            .heading("Server queue");
        if self.loading {
            return screen.activity("Checking the proxy…", None).build();
        }
        if self.jobs.is_empty() {
            screen = screen.empty_state("No borrow or return jobs yet.");
        } else {
            screen = screen.rows_with_menu(self.jobs.iter().enumerate().map(|(index, job)| {
                let state = state_label(&job.state);
                let state = job.error.as_deref().map_or_else(
                    || state.to_owned(),
                    |error| format!("{state} · {}", compact_message(error, 120)),
                );
                (
                    format!("job.{index}"),
                    job.title.clone(),
                    format!(
                        "{} · {} · {state}",
                        catalog_label(&job.catalog),
                        kind_label(&job.kind)
                    ),
                    Glyph::Circle,
                    if job.state == "hook_failed" {
                        format!("retry-hook.{index}")
                    } else {
                        String::new()
                    },
                )
            }));
        }
        if let Some(note) = &self.note {
            screen = screen.banner(BannerLevel::Attention, note.clone());
        }
        screen.build()
    }

    fn settings_screen(&self) -> Screen {
        let mut screen = Self::nav(ScreenBuilder::new("settings").top_bar("Settings"), 3)
            .heading("Proxy settings")
            .facts([
                ("Proxy", self.health.as_deref().unwrap_or("Not checked")),
                ("LCPL storage", "Home server only"),
                ("Credentials", "Never sent to Kobo"),
            ])
            .button(REFRESH, "Check proxy health");
        if let Some(note) = &self.note {
            screen = screen.banner(BannerLevel::Info, note.clone());
        }
        screen.build()
    }

    fn filtered_books(&self) -> Vec<Book> {
        self.books
            .iter()
            .filter(|book| match self.filter {
                CatalogFilter::All => true,
                CatalogFilter::Montreal => book.catalog == "montreal",
                CatalogFilter::Banq => book.catalog == "banq",
            })
            .cloned()
            .collect()
    }

    fn start_search(&mut self, context: &mut Context) {
        if self.inflight.is_some() {
            return;
        }
        let query = self.query.trim().to_owned();
        if query.is_empty() {
            self.note = Some("Enter a title, author, or ISBN.".to_owned());
            self.view = View::Search;
            self.show(context);
            return;
        }
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

    fn start_jobs(&mut self, context: &mut Context) {
        // The server owns the durable queue. Fetch it even on a fresh install
        // where the local UI-state store has no remembered job IDs yet.
        if self.inflight.is_some() {
            self.loading = false;
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
        self.loading = true;
        self.view = View::Queue;
        if let Some(task_id) = context.spawn(task) {
            self.inflight = Some((task_id, RequestKind::Borrow));
        } else {
            self.loading = false;
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
        self.loading = true;
        self.view = View::Queue;
        if let Some(task_id) = context.spawn(task) {
            self.inflight = Some((task_id, RequestKind::Return));
        } else {
            self.loading = false;
            self.note = Some("The Kobo runtime is busy. The return was not sent.".to_owned());
        }
        self.show(context);
    }

    fn start_hook_retry(&mut self, context: &mut Context, index: usize) {
        let Some(job) = self.jobs.get(index) else {
            return;
        };
        if job.state != "hook_failed" || self.inflight.is_some() {
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
        if let Some(task_id) = context.spawn(task) {
            self.inflight = Some((task_id, RequestKind::RetryHook));
        } else {
            self.loading = false;
            self.note = Some("The Kobo runtime is busy. Try again in a moment.".to_owned());
        }
        self.show(context);
    }

    fn schedule_queue_poll(&mut self, context: &mut Context) {
        if self.sleep_task.is_none()
            && self.inflight.is_none()
            && self.jobs.iter().any(|job| is_active_state(&job.state))
        {
            self.sleep_task = context.spawn(Task::Sleep { seconds: 2 });
        }
    }

    #[allow(clippy::single_match_else)]
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
            RequestKind::Books => match parse_books(body) {
                Some(books) => {
                    self.books = books;
                    self.loading = false;
                    self.note = None;
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
                    self.note = None;
                    if self.jobs.iter().any(|job| is_active_state(&job.state)) {
                        self.schedule_queue_poll(context);
                    }
                    if self.jobs.iter().any(|job| job.state == "returned") {
                        self.start_books(context);
                    }
                }
                None => {
                    self.loading = false;
                    self.note = Some("The proxy returned an unreadable queue response.".to_owned());
                }
            },
            RequestKind::Health => match parse_health(body) {
                Some(health) => {
                    self.health = Some(health);
                    self.loading = false;
                    self.note = None;
                }
                None => {
                    self.loading = false;
                    self.note =
                        Some("The proxy returned an unreadable health response.".to_owned());
                }
            },
            RequestKind::Borrow | RequestKind::Return | RequestKind::RetryHook => {
                match parse_job(body) {
                    Some(job) => {
                        if !self.job_ids.iter().any(|id| id == &job.id) {
                            self.job_ids.insert(0, job.id.clone());
                            self.job_ids.truncate(MAX_JOBS);
                            self.save_state(context);
                        }
                        self.jobs.insert(0, job);
                        self.jobs.truncate(MAX_JOBS);
                        self.loading = false;
                        self.note = Some(match kind {
                            RequestKind::Borrow => {
                                "The server accepted the borrow. The LCPL stays at home; nothing was downloaded to this Kobo."
                                    .to_owned()
                            }
                            RequestKind::Return => {
                                "The server accepted the return. This Kobo never held the LCPL."
                                    .to_owned()
                            }
                            RequestKind::RetryHook => {
                                "The server accepted the hook retry. The LCPL stays on the home server."
                                    .to_owned()
                            }
                            _ => "The server accepted the request.".to_owned(),
                        });
                        self.schedule_queue_poll(context);
                    }
                    None => {
                        self.loading = false;
                        self.note =
                            Some("The proxy returned an unreadable job response.".to_owned());
                    }
                }
            }
        }
        self.show(context);
    }

    fn handle_failed(&mut self, context: &mut Context, kind: RequestKind, error: TaskError) {
        self.loading = false;
        self.note = Some(match kind {
            RequestKind::Borrow | RequestKind::Return => match error {
                TaskError::Offline => {
                    "The Kobo is offline. The state-changing request was not retried.".to_owned()
                }
                _ => format!("Request not accepted: {}", Failure::of(error).advice),
            },
            _ => Failure::of(error).advice.to_owned(),
        });
        self.show(context);
    }

    fn back(&mut self, context: &mut Context) {
        self.view = match self.view {
            View::Search | View::Results | View::Library | View::Queue | View::Settings => {
                View::Search
            }
            View::Detail => View::Results,
            View::ConfirmBorrow => View::Detail,
            View::ConfirmReturn => View::Library,
        };
        self.note = None;
        self.show(context);
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
        if action == action_id(BACK) {
            self.back(context);
            return;
        }
        if action == action_id(SEARCH) {
            self.view = View::Search;
            self.show(context);
            return;
        }
        if action == action_id(LIBRARY) {
            self.view = View::Library;
            self.start_books(context);
            return;
        }
        if action == action_id(QUEUE) {
            self.view = View::Queue;
            self.start_jobs(context);
            return;
        }
        if action == action_id(SETTINGS) {
            self.view = View::Settings;
            self.start_health(context);
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
            match self.view {
                View::Search | View::Results => self.start_search(context),
                View::Library => self.start_books(context),
                View::Queue => self.start_jobs(context),
                View::Settings => self.start_health(context),
                _ => {}
            }
            return;
        }
        if action == action_id(CANCEL) {
            self.back(context);
            return;
        }
        if action == action_id(CONFIRM_BORROW) {
            self.start_borrow(context);
            return;
        }
        if action == action_id(CONFIRM_RETURN) {
            self.start_return(context);
            return;
        }
        if let Some(index) = action_index(action, RETRY_HOOK, self.jobs.len()) {
            self.start_hook_retry(context, index);
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
                self.view = View::ConfirmBorrow;
                self.show(context);
            } else {
                self.note = Some("That copy is not currently available.".to_owned());
                self.show(context);
            }
            return;
        }
        let filtered_book_count = self.filtered_books().len();
        if let Some(index) = action_index(action, "book", filtered_book_count) {
            self.selected_book = Some(index);
            self.view = View::ConfirmReturn;
            self.note = None;
            self.show(context);
        }
    }

    fn on_task(&mut self, context: &mut Context, task: TaskId, outcome: TaskOutcome) {
        if self.sleep_task == Some(task) {
            self.sleep_task = None;
            if self.view == View::Queue {
                self.start_jobs(context);
            }
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
                self.loading = false;
                self.note = Some("The request was cancelled.".to_owned());
                self.show(context);
            }
        }
    }

    fn on_foreground(&mut self, context: &mut Context) {
        if self.view == View::Queue && self.inflight.is_none() && self.sleep_task.is_none() {
            self.start_jobs(context);
        }
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
        compact = compact.chars().take(limit.saturating_sub(1)).collect();
        compact.push('…');
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
        _ => kind,
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
        "auth_required" => "Authentication needed",
        "borrowing" => "Borrowing…",
        "downloading" => "Saving LCPL…",
        "stored" => "Saved on server",
        "hook_running" => "Sending…",
        "complete" => "Complete",
        "hook_failed" => "Send failed · retry on queue",
        "borrow_uncertain" => "Borrow uncertain · check account",
        "returning" => "Returning…",
        "returned" => "Returned",
        "return_failed" => "Return failed · loan retained",
        "return_uncertain" => "Return uncertain · check account",
        "failed" => "Failed",
        _ => state,
    }
}

fn is_active_state(state: &str) -> bool {
    matches!(
        state,
        "queued" | "borrowing" | "downloading" | "stored" | "hook_running" | "returning"
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

fn parse_health(body: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(body).ok()?;
    let value = kobo_json::parse(text).ok()?;
    let catalogs = value.get("catalogs")?.as_array()?;
    let summary = catalogs
        .iter()
        .filter_map(|catalog| {
            Some(format!(
                "{} {}",
                catalog_label(catalog.get("catalog")?.as_str()?),
                state_label(catalog.get("state")?.as_str()?)
            ))
        })
        .collect::<Vec<_>>()
        .join(" · ");
    Some(summary)
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
        parse_catalog_status_note, parse_job, parse_search, state_label, Book, Job, PretNumerique,
        SearchResult, Source, View, API, CONFIRM_BORROW, CONFIRM_RETURN,
    };
    use kobo_sdk::{action_id, AppRunner, Command, Context, KoboApp, StoreResult, Task};

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
        assert_eq!(
            state_label("return_failed"),
            "Return failed · loan retained"
        );
    }

    #[test]
    fn job_parser_does_not_require_a_publication_url() {
        let job = parse_job(
            br#"{"id":"job-id","kind":"return","state":"returned","catalog":"banq","title":"Loan"}"#,
        )
        .expect("valid job response");
        assert_eq!(job.kind, "return");
        assert_eq!(job.state, "returned");
    }

    #[test]
    fn labels_turn_catalog_values_into_reader_copy() {
        assert_eq!(kind_label("return"), "Return");
        assert_eq!(
            availability_label(&Source {
                handle: "opaque".to_owned(),
                catalog: "montreal".to_owned(),
                catalog_name: "Montréal".to_owned(),
                availability: "available".to_owned(),
                available: true,
            }),
            "Available now"
        );
    }

    #[test]
    fn authentication_required_jobs_wait_for_an_explicit_refresh() {
        assert!(!is_active_state("auth_required"));
        assert!(is_active_state("downloading"));
        assert!(is_active_state("returning"));
    }

    #[test]
    fn queue_rows_keep_a_bounded_server_error_visible() {
        let app = PretNumerique {
            view: View::Queue,
            jobs: vec![Job {
                id: "job-id".to_owned(),
                kind: "borrow".to_owned(),
                state: "failed".to_owned(),
                title: "A title".to_owned(),
                catalog: "montreal".to_owned(),
                error: Some("The server returned a very long explanation that should remain readable on the panel without becoming an unbounded layout line.".to_owned()),
            }],
            ..PretNumerique::default()
        };
        let screen = format!("{:?}", app.queue_screen());
        assert!(screen.contains("Failed · The server returned a very long"));
        assert!(!screen.contains("unbounded layout line"));
    }

    #[test]
    fn compact_message_collapses_whitespace_and_adds_an_ellipsis() {
        assert_eq!(compact_message("one\n two   three", 12), "one two thr…");
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
        let confirmation = format!("{:?}", app.confirm_return_screen());
        assert!(confirmation.contains("Return this loan?"));
        assert!(confirmation.contains("BAnQ"));
        app.filter = super::CatalogFilter::Montreal;
        assert!(!format!("{:?}", app.library_screen()).contains("Fixture loan"));
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
            sources: vec![Source {
                handle: "opaque-handle".to_owned(),
                catalog: "montreal".to_owned(),
                catalog_name: "Montréal".to_owned(),
                availability: "available".to_owned(),
                available: true,
            }],
        };
        let mut borrow = AppRunner::new(PretNumerique {
            results: vec![result],
            selected_result: Some(0),
            ..PretNumerique::default()
        });
        borrow.start();
        borrow.action(action_id("source.0"));
        assert_eq!(borrow.app().view, View::ConfirmBorrow);
        let borrow_command = borrow
            .action(action_id(CONFIRM_BORROW))
            .into_iter()
            .find_map(|command| match command {
                Command::Spawn {
                    work:
                        Task::Post {
                            url,
                            body,
                            content_type: _,
                            credential: _,
                            headers: _,
                            max_bytes: _,
                        },
                    ..
                } => Some((url, body)),
                _ => None,
            })
            .expect("borrow should post a proxy job");
        assert_eq!(borrow_command.0, format!("{API}/jobs"));
        assert!(borrow_command.1.contains("opaque-handle"));
        assert!(!borrow_command.1.contains("https://"));

        let mut returning = AppRunner::new(PretNumerique {
            view: View::Library,
            books: vec![Book {
                id: "book-id".to_owned(),
                title: "Fixture loan".to_owned(),
                catalog: "banq".to_owned(),
                file_name: "Fixture.lcpl".to_owned(),
                return_state: None,
            }],
            ..PretNumerique::default()
        });
        returning.start();
        returning.action(action_id("book.0"));
        assert_eq!(returning.app().view, View::ConfirmReturn);
        let return_url = returning
            .action(action_id(CONFIRM_RETURN))
            .into_iter()
            .find_map(|command| match command {
                Command::Spawn {
                    work:
                        Task::Post {
                            url,
                            body: _,
                            content_type: _,
                            credential: _,
                            headers: _,
                            max_bytes: _,
                        },
                    ..
                } => Some(url),
                _ => None,
            })
            .expect("return should post to the proxy");
        assert_eq!(return_url, format!("{API}/books/book-id/return"));
    }

    #[test]
    fn state_store_is_the_only_startup_read() {
        let mut app = PretNumerique::default();
        let mut context = Context::default();
        app.on_start(&mut context);
        app.on_store(
            &mut context,
            StoreResult::Loaded {
                key: "ui-state".to_owned(),
                value: Some(b"Dune\njob-1\n".to_vec()),
            },
        );
        assert!(app.loaded_state);
        assert_eq!(app.query, "Dune");
        assert_eq!(app.job_ids, vec!["job-1"]);
    }
}
