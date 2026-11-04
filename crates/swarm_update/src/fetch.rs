//! Downloads with `NSURLSession` (ADR 0010). The system does the TLS and honours
//! the proxy settings of the Mac.

use std::path::Path;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use block2::{DynBlock, RcBlock};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{AnyThread as _, DefinedClass as _, define_class, msg_send};
use objc2_foundation::{
    NSData, NSDictionary, NSError, NSHTTPCookieAcceptPolicy, NSHTTPURLResponse, NSObject,
    NSObjectProtocol, NSString, NSURL, NSURLRequest, NSURLRequestCachePolicy, NSURLResponse,
    NSURLSession, NSURLSessionConfiguration, NSURLSessionDelegate, NSURLSessionTask,
    NSURLSessionTaskDelegate, ns_string,
};

use crate::{Error, Result};

/// The manifest is small: it has 30 seconds, all in all.
const MANIFEST_SECONDS: f64 = 30.0;

/// A download stops when no byte arrives for this long.
const STALL_SECONDS: f64 = 60.0;

/// The most time a whole download may take.
const DOWNLOAD_SECONDS: f64 = 3600.0;

/// How long to wait for the system after its own time limit has passed.
const GRACE: Duration = Duration::from_secs(30);

/// Reads from the network. A trait so that tests and the app can swap it.
pub trait Fetcher {
    /// The body of `url`. Blocks.
    fn get(&self, url: &str, user_agent: &str) -> Result<Vec<u8>>;

    /// Saves the body of `url` as the file `dest`. Blocks.
    fn download(&self, url: &str, user_agent: &str, dest: &Path) -> Result<()>;
}

/// The real fetcher: `NSURLSession`, `https` only.
///
/// Every call has its own ephemeral session: no cookies, no cache, no
/// credentials. The only header it sets is `User-Agent`. A redirect to
/// anything but `https` is refused.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemFetcher {
    allow_loopback: bool,
}

impl SystemFetcher {
    pub fn new() -> Self {
        Self::default()
    }

    /// Also allows `http://127.0.0.1`, for tests with a server on this Mac.
    #[cfg(test)]
    pub(crate) fn loopback() -> Self {
        Self {
            allow_loopback: true,
        }
    }

    fn request(self, url: &str, seconds: f64) -> Result<Retained<NSURLRequest>> {
        let parsed = NSURL::URLWithString(&NSString::from_str(url))
            .ok_or_else(|| Error::Fetch("the URL is not valid".to_owned()))?;

        if !url_allowed(&parsed, self.allow_loopback) {
            return Err(Error::Fetch("the URL is not https".to_owned()));
        }

        Ok(NSURLRequest::requestWithURL_cachePolicy_timeoutInterval(
            &parsed,
            NSURLRequestCachePolicy::ReloadIgnoringLocalCacheData,
            seconds,
        ))
    }

    fn session(
        self,
        user_agent: &str,
        request_seconds: f64,
        resource_seconds: f64,
    ) -> (Retained<NSURLSession>, Retained<RedirectGuard>) {
        let configuration = NSURLSessionConfiguration::ephemeralSessionConfiguration();
        configuration.setRequestCachePolicy(NSURLRequestCachePolicy::ReloadIgnoringLocalCacheData);
        configuration.setTimeoutIntervalForRequest(request_seconds);
        configuration.setTimeoutIntervalForResource(resource_seconds);
        configuration.setHTTPShouldSetCookies(false);
        configuration.setHTTPCookieAcceptPolicy(NSHTTPCookieAcceptPolicy::Never);
        configuration.setHTTPCookieStorage(None);
        configuration.setURLCredentialStorage(None);
        configuration.setURLCache(None);

        let agent = NSString::from_str(user_agent);
        let headers = NSDictionary::from_slices(&[ns_string!("User-Agent")], &[&*agent]);
        // SAFETY: both the key and the value are strings, as header fields need. The
        // untyped dictionary is the same object.
        unsafe {
            let headers = Retained::cast_unchecked::<NSDictionary>(headers);
            configuration.setHTTPAdditionalHeaders(Some(&headers));
        }

        let guard = RedirectGuard::new(self.allow_loopback);
        let delegate: &ProtocolObject<dyn NSURLSessionDelegate> = ProtocolObject::from_ref(&*guard);
        // SAFETY: the delegate lives until the session is invalidated; no queue means
        // the session makes its own serial queue.
        let session = unsafe {
            NSURLSession::sessionWithConfiguration_delegate_delegateQueue(
                &configuration,
                Some(delegate),
                None,
            )
        };

        (session, guard)
    }
}

impl Fetcher for SystemFetcher {
    fn get(&self, url: &str, user_agent: &str) -> Result<Vec<u8>> {
        let request = self.request(url, MANIFEST_SECONDS)?;
        let (session, guard) = self.session(user_agent, MANIFEST_SECONDS, MANIFEST_SECONDS);

        let (sender, receiver) = mpsc::channel();
        let handler = RcBlock::new(
            move |data: *mut NSData, response: *mut NSURLResponse, error: *mut NSError| {
                // SAFETY: Foundation passes objects that live for this call, or null.
                let outcome = unsafe { data_outcome(data, response, error) };
                if sender.send(outcome).is_err() {
                    log::debug!("nobody waits for the manifest any more");
                }
            },
        );
        // SAFETY: the block is a sound completion handler; it only sends on a channel.
        let task = unsafe { session.dataTaskWithRequest_completionHandler(&request, &handler) };
        task.resume();

        let outcome = receiver.recv_timeout(Duration::from_secs_f64(MANIFEST_SECONDS) + GRACE);
        session.invalidateAndCancel();

        finish(outcome, &guard)
    }

    fn download(&self, url: &str, user_agent: &str, dest: &Path) -> Result<()> {
        let request = self.request(url, STALL_SECONDS)?;
        let (session, guard) = self.session(user_agent, STALL_SECONDS, DOWNLOAD_SECONDS);

        let (sender, receiver) = mpsc::channel();
        let dest = dest.to_owned();
        let handler = RcBlock::new(
            move |location: *mut NSURL, response: *mut NSURLResponse, error: *mut NSError| {
                // SAFETY: Foundation passes objects that live for this call, or null. The
                // file at `location` is removed when this block returns, so it is
                // moved here.
                let outcome = unsafe { download_outcome(location, response, error, &dest) };
                if sender.send(outcome).is_err() {
                    log::debug!("nobody waits for the download any more");
                }
            },
        );
        // SAFETY: the block is a sound completion handler; it only moves a file and
        // sends on a channel.
        let task = unsafe { session.downloadTaskWithRequest_completionHandler(&request, &handler) };
        task.resume();

        let outcome = receiver.recv_timeout(Duration::from_secs_f64(DOWNLOAD_SECONDS) + GRACE);
        session.invalidateAndCancel();

        finish(outcome, &guard)
    }
}

/// What the completion handler found: the result, or the reason it failed.
type Outcome<T> = std::result::Result<T, String>;

fn finish<T>(
    outcome: std::result::Result<Outcome<T>, mpsc::RecvTimeoutError>,
    guard: &RedirectGuard,
) -> Result<T> {
    match outcome {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(_)) if guard.ivars().refused.load(Ordering::SeqCst) => Err(Error::Fetch(
            "the server redirected to a URL that is not https".to_owned(),
        )),
        Ok(Err(reason)) => Err(Error::Fetch(reason)),
        Err(_) => Err(Error::Fetch("the server did not answer in time".to_owned())),
    }
}

/// # Safety
///
/// Each pointer is null or points to a live object of its type.
unsafe fn data_outcome(
    data: *mut NSData,
    response: *mut NSURLResponse,
    error: *mut NSError,
) -> Outcome<Vec<u8>> {
    // SAFETY: the caller promises the pointers are null or live.
    let (data, response, error) = unsafe { (data.as_ref(), response.as_ref(), error.as_ref()) };

    check_answer(response, error)?;

    Ok(data.map(NSData::to_vec).unwrap_or_default())
}

/// # Safety
///
/// Each pointer is null or points to a live object of its type.
unsafe fn download_outcome(
    location: *mut NSURL,
    response: *mut NSURLResponse,
    error: *mut NSError,
    dest: &Path,
) -> Outcome<()> {
    // SAFETY: the caller promises the pointers are null or live.
    let (location, response, error) =
        unsafe { (location.as_ref(), response.as_ref(), error.as_ref()) };

    check_answer(response, error)?;

    let from = location
        .and_then(NSURL::path)
        .map(|path| path.to_string())
        .ok_or("the download has no file")?;

    move_file(Path::new(&from), dest).map_err(|error| format!("cannot save the download: {error}"))
}

/// The error of the system, or a status outside 200 to 299, as a reason.
fn check_answer(
    response: Option<&NSURLResponse>,
    error: Option<&NSError>,
) -> std::result::Result<(), String> {
    if let Some(error) = error {
        return Err(error.localizedDescription().to_string());
    }

    let status = response
        .and_then(|response| response.downcast_ref::<NSHTTPURLResponse>())
        .map(NSHTTPURLResponse::statusCode)
        .ok_or("the answer is not HTTP")?;

    if (200..300).contains(&status) {
        Ok(())
    } else {
        Err(format!("the server answered with status {status}"))
    }
}

/// Renames, or copies when the two folders are on different volumes.
fn move_file(from: &Path, to: &Path) -> std::io::Result<()> {
    if std::fs::rename(from, to).is_ok() {
        return Ok(());
    }

    std::fs::copy(from, to).map(|_| ()).inspect_err(|_| {
        std::fs::remove_file(to).ok();
    })
}

fn url_allowed(url: &NSURL, allow_loopback: bool) -> bool {
    let scheme = url.scheme().map(|scheme| scheme.to_string());
    let host = url.host().map(|host| host.to_string());

    scheme_allowed(scheme.as_deref(), host.as_deref(), allow_loopback)
}

/// `https` always. `http` only to this Mac, and only when `allow_loopback` is set.
fn scheme_allowed(scheme: Option<&str>, host: Option<&str>, allow_loopback: bool) -> bool {
    match scheme {
        Some(scheme) if scheme.eq_ignore_ascii_case("https") => true,
        Some(scheme) if scheme.eq_ignore_ascii_case("http") => {
            allow_loopback && host == Some("127.0.0.1")
        }
        _ => false,
    }
}

struct GuardIvars {
    allow_loopback: bool,
    refused: AtomicBool,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements and the class has no `Drop`.
    #[unsafe(super(NSObject))]
    #[ivars = GuardIvars]
    struct RedirectGuard;

    // SAFETY: `NSObjectProtocol` has no extra requirements.
    unsafe impl NSObjectProtocol for RedirectGuard {}

    // SAFETY: all methods of the protocol are optional.
    unsafe impl NSURLSessionDelegate for RedirectGuard {}

    // SAFETY: the signature is the one of the protocol; the block is called once.
    unsafe impl NSURLSessionTaskDelegate for RedirectGuard {
        #[unsafe(method(URLSession:task:willPerformHTTPRedirection:newRequest:completionHandler:))]
        fn will_redirect(
            &self,
            _session: &NSURLSession,
            _task: &NSURLSessionTask,
            _response: &NSHTTPURLResponse,
            request: &NSURLRequest,
            completion_handler: &DynBlock<dyn Fn(*mut NSURLRequest)>,
        ) {
            let allowed = request
                .URL()
                .is_some_and(|url| url_allowed(&url, self.ivars().allow_loopback));

            if allowed {
                completion_handler.call((ptr::from_ref(request).cast_mut(),));
            } else {
                self.ivars().refused.store(true, Ordering::SeqCst);
                completion_handler.call((ptr::null_mut(),));
            }
        }
    }
);

impl RedirectGuard {
    fn new(allow_loopback: bool) -> Retained<Self> {
        let this = Self::alloc().set_ivars(GuardIvars {
            allow_loopback,
            refused: AtomicBool::new(false),
        });

        // SAFETY: `init` of NSObject, with the ivars set.
        unsafe { msg_send![super(this), init] }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{Reply, TempFolder, TestServer};

    const AGENT: &str = "Swarm/9.9.9";

    #[test]
    fn get_returns_the_body() {
        let server = TestServer::start([("/m.json", Reply::Body(b"hello".to_vec()))]);

        let body = SystemFetcher::loopback().get(&server.url("/m.json"), AGENT);

        assert_eq!(body.expect("body"), b"hello");
    }

    #[test]
    fn get_sends_the_user_agent_and_no_cookie_or_credential() {
        let server = TestServer::start([("/m.json", Reply::Body(b"x".to_vec()))]);

        SystemFetcher::loopback()
            .get(&server.url("/m.json"), AGENT)
            .expect("body");

        let head = server.requests().remove(0).to_ascii_lowercase();
        assert!(head.contains("user-agent: swarm/9.9.9\r\n"), "{head}");
        for header in ["cookie", "authorization", "referer", "origin"] {
            assert!(!head.contains(&format!("{header}:")), "{head}");
        }
    }

    #[test]
    fn get_refuses_a_status_outside_200_to_299() {
        let server = TestServer::start([("/gone", Reply::Status(404))]);

        let error = SystemFetcher::loopback().get(&server.url("/gone"), AGENT);

        assert!(error.expect_err("refused").to_string().contains("404"));
    }

    #[test]
    fn get_follows_a_redirect_to_an_allowed_url() {
        let server = TestServer::start([
            ("/latest", Reply::Redirect("/real".to_owned())),
            ("/real", Reply::Body(b"found".to_vec())),
        ]);

        let body = SystemFetcher::loopback().get(&server.url("/latest"), AGENT);

        assert_eq!(body.expect("body"), b"found");
    }

    #[test]
    fn get_refuses_a_redirect_to_a_url_that_is_not_https() {
        let server = TestServer::start([(
            "/latest",
            Reply::Redirect("http://192.0.2.1/evil".to_owned()),
        )]);

        let error = SystemFetcher::loopback().get(&server.url("/latest"), AGENT);

        assert!(
            error
                .expect_err("refused")
                .to_string()
                .contains("not https")
        );
    }

    #[test]
    fn the_real_fetcher_refuses_a_url_that_is_not_https_without_asking() {
        let server = TestServer::start([("/m.json", Reply::Body(b"x".to_vec()))]);

        let error = SystemFetcher::new().get(&server.url("/m.json"), AGENT);

        assert!(error.is_err());
        assert!(server.requests().is_empty());
    }

    #[test]
    fn download_saves_the_body_to_the_file() {
        let server = TestServer::start([("/a.zip", Reply::Body(vec![7; 100_000]))]);
        let folder = TempFolder::new("download");
        let dest = folder.path().join("a.zip");

        SystemFetcher::loopback()
            .download(&server.url("/a.zip"), AGENT, &dest)
            .expect("download");

        assert_eq!(std::fs::read(dest).expect("file"), vec![7; 100_000]);
    }

    #[test]
    fn download_refuses_a_bad_status_and_leaves_no_file() {
        let server = TestServer::start([("/a.zip", Reply::Status(500))]);
        let folder = TempFolder::new("download-bad");
        let dest = folder.path().join("a.zip");

        let result = SystemFetcher::loopback().download(&server.url("/a.zip"), AGENT, &dest);

        assert!(result.is_err());
        assert!(!dest.exists());
    }

    #[test]
    fn only_https_and_local_http_pass_the_scheme_check() {
        assert!(scheme_allowed(Some("https"), Some("github.com"), false));
        assert!(scheme_allowed(Some("HTTPS"), None, false));
        assert!(!scheme_allowed(Some("http"), Some("127.0.0.1"), false));
        assert!(scheme_allowed(Some("http"), Some("127.0.0.1"), true));
        assert!(!scheme_allowed(Some("http"), Some("example.com"), true));
        assert!(!scheme_allowed(Some("file"), None, true));
        assert!(!scheme_allowed(None, None, true));
    }

    #[test]
    fn a_file_is_moved_when_the_folders_match() {
        let folder = TempFolder::new("move");
        std::fs::write(folder.path().join("from"), "x").expect("file");

        move_file(&folder.path().join("from"), &folder.path().join("to")).expect("move");

        assert!(!folder.path().join("from").exists());
        assert!(folder.path().join("to").exists());
    }
}
