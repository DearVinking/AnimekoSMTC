use animeko_protocol::Media;
use anyhow::{bail, ensure, Context, Result};
use objc2_foundation::{NSString, NSURLComponents, NSURL};
use std::{
    net::IpAddr,
    time::{Duration, Instant},
};

const MAX_BYTES: usize = 8 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(10);

fn validate_url(url: &NSURL) -> Result<()> {
    let parts = NSURLComponents::componentsWithURL_resolvingAgainstBaseURL(url, false)
        .context("无效的封面 URL")?;
    ensure!(
        parts
            .scheme()
            .is_some_and(|v| v.to_string().eq_ignore_ascii_case("https")),
        "封面只允许 HTTPS"
    );
    ensure!(
        parts.user().is_none() && parts.password().is_none(),
        "封面 URL 不允许用户凭据"
    );
    let host = parts
        .host()
        .context("封面 URL 缺少主机")?
        .to_string()
        .to_lowercase();
    let host = host.trim_end_matches('.');
    ensure!(
        !host.is_empty() && host != "localhost" && !host.ends_with(".localhost"),
        "封面不允许本机地址"
    );
    ensure!(!host.contains('%'), "封面不允许带网络区域的地址");
    let numeric_ipv4 = std::ffi::CString::new(host).ok().and_then(|host| {
        unsafe extern "C" {
            fn inet_aton(cp: *const std::ffi::c_char, addr: *mut libc::in_addr) -> std::ffi::c_int;
        }
        let mut address = libc::in_addr { s_addr: 0 };
        (unsafe { inet_aton(host.as_ptr(), &mut address) } != 0)
            .then(|| std::net::Ipv4Addr::from(address.s_addr.to_ne_bytes()))
    });
    if let Some(address) = numeric_ipv4 {
        ensure!(
            !local_address(IpAddr::V4(address)),
            "封面不允许本机或私有网络地址"
        );
    }
    if let Ok(address) = host.trim_matches(['[', ']']).parse::<IpAddr>() {
        ensure!(!local_address(address), "封面不允许本机或私有网络地址");
    }
    Ok(())
}

fn local_address(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(ip) => {
            ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_unspecified()
                || ip.is_multicast()
                || ip.is_broadcast()
        }
        IpAddr::V6(ip) => {
            ip.is_loopback()
                || ip.is_unspecified()
                || ip.is_multicast()
                || ip.segments()[0] & 0xfe00 == 0xfc00
                || ip.segments()[0] & 0xffc0 == 0xfe80
                || ip.to_ipv4().is_some_and(|v4| local_address(IpAddr::V4(v4)))
        }
    }
}

#[derive(Default)]
struct DownloadState {
    bytes: Vec<u8>,
    redirects: u8,
    terminal: bool,
}
impl DownloadState {
    fn push(&mut self, incoming: &[u8]) -> Result<()> {
        ensure!(!self.terminal, "封面请求已结束");
        if !self
            .bytes
            .len()
            .checked_add(incoming.len())
            .is_some_and(|size| size <= MAX_BYTES)
        {
            self.terminal = true;
            bail!("封面超过 8 MiB");
        }
        self.bytes.extend_from_slice(incoming);
        Ok(())
    }
    fn redirect(&mut self, url: &NSURL) -> Result<()> {
        ensure!(!self.terminal, "封面请求已结束");
        let result = validate_url(url).and_then(|()| {
            ensure!(self.redirects < 5, "封面重定向超过 5 次");
            self.redirects += 1;
            Ok(())
        });
        if result.is_err() {
            self.terminal = true;
        }
        result
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ArtworkKey {
    subject_id: i64,
    episode_id: i64,
    url: String,
    generation: u64,
}
struct Transfer {
    key: ArtworkKey,
    task_id: usize,
    started: Instant,
    download: DownloadState,
    completion: Option<Result<Vec<u8>>>,
}
impl Transfer {
    fn finish(&mut self, result: Result<Vec<u8>>) {
        self.download.terminal = true;
        self.download.bytes.clear();
        self.completion = Some(result);
    }
}

#[derive(Default)]
struct LoaderState {
    key: Option<ArtworkKey>,
    generation: u64,
    transfer: Option<Transfer>,
}
impl LoaderState {
    fn select(&mut self, media: Option<&Media>) -> bool {
        let selected = media.filter(|m| !m.cover_url.is_empty());
        if self
            .key
            .as_ref()
            .map(|k| (k.subject_id, k.episode_id, k.url.as_str()))
            == selected.map(|m| (m.subject_id, m.episode_id, m.cover_url.as_str()))
        {
            return false;
        }
        self.generation += 1;
        self.key = selected.map(|m| ArtworkKey {
            subject_id: m.subject_id,
            episode_id: m.episode_id,
            url: m.cover_url.clone(),
            generation: self.generation,
        });
        self.transfer = None;
        true
    }
    fn begin(&mut self, task_id: usize, now: Instant) {
        self.transfer = self.key.as_ref().map(|key| Transfer {
            key: key.clone(),
            task_id,
            started: now,
            download: DownloadState::default(),
            completion: None,
        });
    }
    fn apply(
        &mut self,
        task_id: usize,
        now: Instant,
        operation: impl FnOnce(&mut Transfer) -> Result<()>,
    ) -> bool {
        let Some(transfer) = self
            .transfer
            .as_mut()
            .filter(|t| t.task_id == task_id && !t.download.terminal)
        else {
            return false;
        };
        let result = if now.saturating_duration_since(transfer.started) >= TIMEOUT {
            Err(anyhow::anyhow!("封面下载超过 10 秒"))
        } else {
            operation(transfer)
        };
        if let Err(error) = result {
            transfer.finish(Err(error));
            return false;
        }
        true
    }
    fn take(&mut self, now: Instant) -> Option<Result<Vec<u8>>> {
        if let Some(transfer) = &self.transfer {
            if !transfer.download.terminal
                && now.saturating_duration_since(transfer.started) >= TIMEOUT
            {
                self.apply(transfer.task_id, now, |_| Ok(()));
            }
        }
        let transfer = self.transfer.as_mut()?;
        if Some(&transfer.key) != self.key.as_ref() {
            return None;
        }
        transfer.completion.take()
    }
}

use objc2::{
    define_class, msg_send, rc::Retained, runtime::ProtocolObject, AnyThread, DefinedClass,
    MainThreadMarker,
};
use objc2_app_kit::NSImage;
use objc2_foundation::{
    NSData, NSError, NSHTTPURLResponse, NSObject, NSObjectProtocol, NSSize, NSURLRequest,
    NSURLRequestCachePolicy, NSURLResponse, NSURLSession, NSURLSessionConfiguration,
    NSURLSessionDataDelegate, NSURLSessionDataTask, NSURLSessionDelegate,
    NSURLSessionResponseDisposition, NSURLSessionTask, NSURLSessionTaskDelegate,
};
use std::{
    panic::{catch_unwind, AssertUnwindSafe},
    sync::Mutex,
};

define_class!(
    #[unsafe(super = NSObject)]
    #[ivars = Mutex<LoaderState>]
    struct ArtworkDelegate;
    unsafe impl NSObjectProtocol for ArtworkDelegate {}
    unsafe impl NSURLSessionDelegate for ArtworkDelegate {}
    unsafe impl NSURLSessionTaskDelegate for ArtworkDelegate {
        #[unsafe(method(URLSession:task:willPerformHTTPRedirection:newRequest:completionHandler:))]
        unsafe fn redirect(&self, _session: &NSURLSession, task: &NSURLSessionTask,
            _response: &NSHTTPURLResponse, request: &NSURLRequest,
            completion: &block2::DynBlock<dyn Fn(*mut NSURLRequest)>) {
            let accepted = self.apply(task, |transfer| {
                let url = request.URL().context("封面重定向缺少 URL")?;
                transfer.download.redirect(&url)
            });
            completion.call((if accepted { std::ptr::from_ref(request).cast_mut() } else { std::ptr::null_mut() },));
            if !accepted { task.cancel(); }
        }
        #[unsafe(method(URLSession:task:didCompleteWithError:))]
        fn completed(&self, _session: &NSURLSession, task: &NSURLSessionTask, error: Option<&NSError>) {
            self.apply(task, |transfer| {
                if let Some(error) = error { bail!("封面网络请求失败（{}）", error.code()); }
                let bytes = std::mem::take(&mut transfer.download.bytes);
                transfer.finish(Ok(bytes));
                Ok(())
            });
        }
    }
    unsafe impl NSURLSessionDataDelegate for ArtworkDelegate {
        #[unsafe(method(URLSession:dataTask:didReceiveResponse:completionHandler:))]
        unsafe fn response(&self, _session: &NSURLSession, task: &NSURLSessionDataTask,
            response: &NSURLResponse, completion: &block2::DynBlock<dyn Fn(NSURLSessionResponseDisposition)>) {
            let accepted = self.apply(task, |_| {
                let http = response.downcast_ref::<NSHTTPURLResponse>().context("封面响应不是 HTTP")?;
                ensure!((200..300).contains(&http.statusCode()), "封面 HTTP 状态为 {}", http.statusCode());
                let url = response.URL().context("封面响应缺少 URL")?;
                validate_url(&url)?;
                ensure!(response.expectedContentLength() <= MAX_BYTES as i64, "封面超过 8 MiB");
                Ok(())
            });
            completion.call((if accepted { NSURLSessionResponseDisposition::Allow } else { NSURLSessionResponseDisposition::Cancel },));
            if !accepted { task.cancel(); }
        }
        #[unsafe(method(URLSession:dataTask:didReceiveData:))]
        fn received_data(&self, _session: &NSURLSession, task: &NSURLSessionDataTask, data: &NSData) {
            let accepted = self.apply(task, |transfer| {
                transfer.download.push(unsafe { data.as_bytes_unchecked() })
            });
            if !accepted { task.cancel(); }
        }
    }
);

impl ArtworkDelegate {
    fn apply(
        &self,
        task: &NSURLSessionTask,
        operation: impl FnOnce(&mut Transfer) -> Result<()>,
    ) -> bool {
        let result = catch_unwind(AssertUnwindSafe(|| {
            self.ivars()
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .apply(task.taskIdentifier(), Instant::now(), operation)
        }));
        match result {
            Ok(accepted) => accepted,
            Err(_) => {
                let mut state = self.ivars().lock().unwrap_or_else(|p| p.into_inner());
                if let Some(transfer) = state
                    .transfer
                    .as_mut()
                    .filter(|t| t.task_id == task.taskIdentifier())
                {
                    transfer.finish(Err(anyhow::anyhow!("封面回调失败")));
                }
                false
            }
        }
    }
}

pub(super) struct ArtworkLoader {
    _main_thread: MainThreadMarker,
    session: Retained<NSURLSession>,
    delegate: Retained<ArtworkDelegate>,
    task: Option<Retained<NSURLSessionDataTask>>,
}
impl ArtworkLoader {
    pub(super) fn new() -> Result<Self> {
        let main_thread = MainThreadMarker::new().context("封面加载器必须在主线程创建")?;
        let delegate = ArtworkDelegate::alloc().set_ivars(Mutex::new(LoaderState::default()));
        let delegate: Retained<ArtworkDelegate> = unsafe { msg_send![super(delegate), init] };
        let config = NSURLSessionConfiguration::ephemeralSessionConfiguration();
        config.setTimeoutIntervalForRequest(10.0);
        config.setTimeoutIntervalForResource(10.0);
        config.setHTTPShouldSetCookies(false);
        config.setHTTPCookieStorage(None);
        config.setURLCredentialStorage(None);
        config.setURLCache(None);
        config.setRequestCachePolicy(NSURLRequestCachePolicy::ReloadIgnoringLocalCacheData);
        let session = unsafe {
            NSURLSession::sessionWithConfiguration_delegate_delegateQueue(
                &config,
                Some(ProtocolObject::from_ref(&*delegate)),
                None,
            )
        };
        Ok(Self {
            _main_thread: main_thread,
            session,
            delegate,
            task: None,
        })
    }

    pub(super) fn select(&mut self, media: Option<&Media>) -> Result<()> {
        let url = {
            let mut state = self
                .delegate
                .ivars()
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            if !state.select(media) {
                return Ok(());
            }
            state.key.as_ref().map(|key| key.url.clone())
        };
        if let Some(task) = self.task.take() {
            task.cancel();
        }
        let Some(url) = url else { return Ok(()) };
        let url = NSURL::URLWithString(&NSString::from_str(&url)).context("无效的封面 URL")?;
        validate_url(&url)?;
        let task = self.session.dataTaskWithURL(&url);
        self.delegate
            .ivars()
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .begin(task.taskIdentifier(), Instant::now());
        task.resume();
        self.task = Some(task);
        Ok(())
    }

    pub(super) fn poll(&mut self) -> Option<Result<Retained<NSImage>>> {
        let result = self
            .delegate
            .ivars()
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take(Instant::now())?;
        if let Some(task) = self.task.take() {
            task.cancel();
        }
        Some(result.and_then(|bytes| decode(&bytes)))
    }
}
impl Drop for ArtworkLoader {
    fn drop(&mut self) {
        self.delegate
            .ivars()
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .select(None);
        if let Some(task) = self.task.take() {
            task.cancel();
        }
        self.session.invalidateAndCancel();
    }
}

fn acceptable_dimensions(width: u64, height: u64) -> bool {
    width > 0
        && height > 0
        && width
            .checked_mul(height)
            .is_some_and(|pixels| pixels <= 16_000_000)
}

fn decode(bytes: &[u8]) -> Result<Retained<NSImage>> {
    use objc2_core_foundation::{CFBoolean, CFData, CFDictionary, CFNumber, CFString, CFType};
    use objc2_image_io::*;
    MainThreadMarker::new().context("封面图像必须在主线程创建")?;
    ensure!(bytes.len() <= MAX_BYTES, "封面超过 8 MiB");
    let data = CFData::from_bytes(bytes);
    unsafe {
        let options = CFDictionary::<CFString, CFType>::from_slices(
            &[kCGImageSourceShouldCache],
            &[CFBoolean::new(false)],
        );
        let source = CGImageSource::with_data(&data, Some(options.as_opaque()))
            .context("无法识别封面图像")?;
        let properties = source
            .properties_at_index(0, Some(options.as_opaque()))
            .context("封面缺少图像属性")?;
        let properties = properties.cast_unchecked::<CFString, CFType>();
        let dimension = |key: &CFString| -> Result<u64> {
            let value = properties.get(key).context("封面缺少尺寸")?;
            let value = value
                .downcast_ref::<CFNumber>()
                .and_then(CFNumber::as_i64)
                .context("无效的封面尺寸")?;
            Ok(u64::try_from(value)?)
        };
        ensure!(
            acceptable_dimensions(
                dimension(kCGImagePropertyPixelWidth)?,
                dimension(kCGImagePropertyPixelHeight)?
            ),
            "封面超过 1600 万像素或尺寸无效"
        );
        let max_size = CFNumber::new_i32(1024);
        let options = CFDictionary::<CFString, CFType>::from_slices(
            &[
                kCGImageSourceShouldCache,
                kCGImageSourceCreateThumbnailFromImageAlways,
                kCGImageSourceThumbnailMaxPixelSize,
                kCGImageSourceCreateThumbnailWithTransform,
            ],
            &[
                CFBoolean::new(false),
                CFBoolean::new(true),
                &max_size,
                CFBoolean::new(true),
            ],
        );
        let image = source
            .thumbnail_at_index(0, Some(options.as_opaque()))
            .context("无法解码封面图像")?;
        Ok(NSImage::initWithCGImage_size(
            NSImage::alloc(),
            &image,
            NSSize::new(
                objc2_core_graphics::CGImage::width(Some(&image)) as f64,
                objc2_core_graphics::CGImage::height(Some(&image)) as f64,
            ),
        ))
    }
}

#[cfg(test)]
pub(super) fn stage_test_download(loader: &mut ArtworkLoader, media: &Media) {
    let task = loader
        .session
        .dataTaskWithURL(&tests::url(&media.cover_url));
    {
        let mut state = loader.delegate.ivars().lock().unwrap();
        state.select(Some(media));
        state.begin(task.taskIdentifier(), Instant::now());
    }
    let data = NSData::with_bytes(include_bytes!(
        "../../../../tests/fixtures/artwork/small.png"
    ));
    loader.delegate.received_data(
        objc2::sel!(URLSession:dataTask:didReceiveData:),
        &loader.session,
        &task,
        &data,
    );
    loader.delegate.completed(
        objc2::sel!(URLSession:task:didCompleteWithError:),
        &loader.session,
        &task,
        None,
    );
    task.cancel();
}

#[cfg(test)]
pub(super) fn run_native_checks() {
    assert!(decode(b"not an image").is_err());
    assert!(decode(include_bytes!(
        "../../../../tests/fixtures/artwork/oversized.png"
    ))
    .is_err());
    for (bytes, width, height) in [
        (
            &include_bytes!("../../../../tests/fixtures/artwork/small.png")[..],
            2.0,
            1.0,
        ),
        (
            &include_bytes!("../../../../tests/fixtures/artwork/wide.png")[..],
            1024.0,
            1.0,
        ),
        (
            &include_bytes!("../../../../tests/fixtures/artwork/animated.gif")[..],
            1.0,
            1.0,
        ),
    ] {
        let image = decode(bytes).unwrap();
        assert_eq!((image.size().width, image.size().height), (width, height));
    }
    let loader = ArtworkLoader::new().unwrap();
    let task = loader
        .session
        .dataTaskWithURL(&tests::url("https://example.com/cover.png"));
    {
        let mut state = loader.delegate.ivars().lock().unwrap();
        state.select(Some(&tests::media(1)));
        state.begin(task.taskIdentifier(), Instant::now());
    }
    let data = NSData::with_bytes(&vec![0; MAX_BYTES]);
    loader.delegate.received_data(
        objc2::sel!(URLSession:dataTask:didReceiveData:),
        &loader.session,
        &task,
        &data,
    );
    loader.delegate.received_data(
        objc2::sel!(URLSession:dataTask:didReceiveData:),
        &loader.session,
        &task,
        &NSData::with_bytes(&[1]),
    );
    loader.delegate.completed(
        objc2::sel!(URLSession:task:didCompleteWithError:),
        &loader.session,
        &task,
        None,
    );
    assert!(loader
        .delegate
        .ivars()
        .lock()
        .unwrap()
        .take(Instant::now())
        .unwrap()
        .is_err());
    assert!(loader
        .delegate
        .ivars()
        .lock()
        .unwrap()
        .take(Instant::now())
        .is_none());
    println!(
        "Verified ImageIO decoding, thumbnail bounds and actual NSData streamed-byte enforcement."
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use animeko_protocol::{Media, Playback};

    pub(super) fn url(value: &str) -> objc2::rc::Retained<NSURL> {
        NSURL::URLWithString(&NSString::from_str(value)).unwrap()
    }
    pub(super) fn media(id: i64) -> Media {
        Media {
            subject_id: 42,
            episode_id: id,
            title: "Test".into(),
            episode: "Episode".into(),
            cover_url: "https://example.com/cover.png".into(),
            playback: Playback::Playing,
            position_ms: 0,
            duration_ms: None,
            playback_rate: 1.0,
            play_when_ready: true,
        }
    }

    #[test]
    fn only_https_without_credentials_or_explicit_local_targets_is_allowed() {
        assert!(validate_url(&url("https://example.com/a.png")).is_ok());
        assert!(validate_url(&url("https://8.8.8.8/a.png")).is_ok());
        for value in [
            "http://example.com/a",
            "file:///tmp/a",
            "https://user:pass@example.com/a",
            "https://localhost/a",
            "https://x.localhost./a",
            "https://127.0.0.1/a",
            "https://10.1.1.1/a",
            "https://172.16.0.1/a",
            "https://192.168.1.1/a",
            "https://127.1/a",
            "https://2130706433/a",
            "https://0x7f000001/a",
            "https://0177.0.0.1/a",
            "https://[fe80::1%25en0]/a",
            "https://169.254.1.1/a",
            "https://[::1]/a",
            "https://[fc00::1]/a",
            "https://[fe80::1]/a",
            "https://[::ffff:127.0.0.1]/a",
            "https://0.0.0.0/a",
        ] {
            assert!(validate_url(&url(value)).is_err(), "{value}");
        }
    }

    #[test]
    fn chunked_download_cannot_exceed_budget() {
        let mut state = DownloadState::default();
        state.push(&vec![0; 8 * 1024 * 1024]).unwrap();
        assert!(state.push(&[1]).is_err());
        assert!(state.terminal);
        assert!(state.push(&[]).is_err());
    }

    #[test]
    fn redirects_revalidate_url_and_have_a_fixed_limit() {
        let mut state = DownloadState::default();
        for _ in 0..5 {
            state.redirect(&url("https://example.com/next")).unwrap();
        }
        assert!(state.redirect(&url("https://example.com/next")).is_err());
        assert!(state.terminal);
        let mut state = DownloadState::default();
        assert!(state.redirect(&url("http://example.com/insecure")).is_err());
        assert!(state.terminal);
    }

    #[test]
    fn replacement_and_clear_discard_late_completion_without_retrying_each_tick() {
        let now = Instant::now();
        let mut state = LoaderState::default();
        assert!(state.select(Some(&media(1))));
        state.begin(10, now);
        let mut progress = media(1);
        progress.position_ms = 500;
        assert!(!state.select(Some(&progress)));
        assert!(state.select(Some(&media(2))));
        state.begin(11, now);
        assert!(!state.apply(10, now, |t| {
            t.finish(Ok(vec![1]));
            Ok(())
        }));
        assert!(state.take(now).is_none());
        assert!(state.apply(11, now, |t| {
            t.finish(Ok(vec![2]));
            Ok(())
        }));
        assert_eq!(state.take(now).unwrap().unwrap(), vec![2]);
        assert!(state.take(now).is_none());
        assert!(!state.select(Some(&media(2))));
        state.select(None);
        assert!(!state.apply(11, now, |t| {
            t.finish(Ok(vec![3]));
            Ok(())
        }));
        assert!(state.take(now).is_none());
    }

    #[test]
    fn total_timeout_does_not_reset_at_redirect_or_completion() {
        let now = Instant::now();
        let mut state = LoaderState::default();
        state.select(Some(&media(1)));
        state.begin(1, now);
        assert!(state.apply(1, now + Duration::from_secs(9), |t| t
            .download
            .redirect(&url("https://example.com/next"))));
        assert!(state.take(now + Duration::from_secs(10)).unwrap().is_err());
        assert!(!state.apply(1, now + Duration::from_secs(11), |t| {
            t.finish(Ok(vec![1]));
            Ok(())
        }));
        assert!(state.take(now + Duration::from_secs(11)).is_none());
    }
    #[test]
    fn image_dimensions_are_nonzero_bounded_and_overflow_safe() {
        assert!(acceptable_dimensions(4000, 4000));
        for (width, height) in [(0, 1), (1, 0), (4001, 4001), (u64::MAX, 2)] {
            assert!(!acceptable_dimensions(width, height));
        }
    }
}
