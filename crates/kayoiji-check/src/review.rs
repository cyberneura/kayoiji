//! 言語モデルの補助プロセス (`helper/english-check.swift`) に、英文の訂正と日本語訳を頼む。
//!
//! 補助プロセスとのやり取りは、すべて専用のスレッドで行う。入力メソッドのメインスレッドは、
//! 補助プロセスの起動も、依頼の書き込みも、答えも待たない。補助プロセスが止まっても、打鍵は止まらない。
//! 答えが `ANSWER_TIMEOUT` を過ぎても来なければ、答えられなかったと知らせる (`Event::Failed`)。

use std::io::{self, BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, Weak};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// 補助プロセスの答え。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Review {
    /// 依頼に付けた番号。打鍵が進んだ後に届いた古い答えを、受け取る側で捨てるために使う。
    pub id: u64,
    /// 訂正した英文。
    pub corrected: String,
    /// 訂正文が、頼んだ英文と実質的に同じかどうか。同じなら、英文に直す所は無い。
    pub unchanged: bool,
    pub translation: String,
}

/// 補助プロセスから届く知らせ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Review(Review),
    /// 言語モデルが、この番号の依頼に答えられなかった。
    Failed { id: u64 },
    /// 言語モデルが使えない、または補助プロセスが終わった。これ以降、答えは届かない。
    Unavailable(String),
}

#[derive(Deserialize)]
struct Hello {
    available: bool,
    #[serde(default)]
    reason: String,
}

#[derive(Deserialize)]
struct Answer {
    id: u64,
    #[serde(default)]
    corrected: String,
    #[serde(default)]
    translation: String,
    error: Option<String>,
}

#[derive(Serialize)]
struct Request<'a> {
    id: u64,
    text: &'a str,
}

/// 依頼してから答えを待つ時間。言語モデルの答えは、ふつう数秒で届く。
const ANSWER_TIMEOUT: Duration = Duration::from_secs(20);

/// 最後に頼んだ依頼。
struct Asked {
    id: u64,
    text: String,
    at: Instant,
    /// まだ答えも、答えられなかったという知らせも出していない。
    waiting: bool,
}

type SharedAsked = Arc<Mutex<Option<Asked>>>;
type OnEvent = Arc<dyn Fn(Event) + Send + Sync>;

/// 起動した補助プロセス。破棄すると、プロセスを終わらせる。
pub struct Reviewer {
    child: Mutex<Child>,
    requests: Mutex<Sender<(u64, String)>>,
    asked: SharedAsked,
    /// 補助プロセスが、言語モデルを使えると知らせてきたかどうか。
    available: Arc<AtomicBool>,
}

impl Reviewer {
    /// 補助プロセスを起動する。補助プロセスからの知らせは、別のスレッドから `on_event` に届く。
    ///
    /// 言語モデルが使えるかどうかは、起動した後に分かる (`is_available`)。ここでは待たない。
    pub fn start(helper: &Path, on_event: impl Fn(Event) + Send + Sync + 'static) -> io::Result<Self> {
        Self::start_with_timeout(helper, ANSWER_TIMEOUT, on_event)
    }

    fn start_with_timeout(
        helper: &Path,
        timeout: Duration,
        on_event: impl Fn(Event) + Send + Sync + 'static,
    ) -> io::Result<Self> {
        let on_event: OnEvent = Arc::new(on_event);
        let mut child = Command::new(helper)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let stdin = child.stdin.take().expect("stdin is piped");
        let stdout = BufReader::new(child.stdout.take().expect("stdout is piped"));
        let asked = SharedAsked::default();
        let available = Arc::new(AtomicBool::new(false));
        let (requests, queue) = mpsc::channel();
        thread::spawn(move || write_requests(stdin, queue));
        let reader = (Arc::clone(&asked), Arc::clone(&available), Arc::clone(&on_event));
        thread::spawn(move || read_answers(stdout, &reader.0, &reader.1, &*reader.2));
        let watched = Arc::downgrade(&asked);
        thread::spawn(move || watch_answers(&watched, timeout, &*on_event));
        Ok(Self {
            child: Mutex::new(child),
            requests: Mutex::new(requests),
            asked,
            available,
        })
    }

    pub fn is_available(&self) -> bool {
        self.available.load(Ordering::Relaxed)
    }

    /// 英文の訂正と訳を頼む。待たずに戻る。答えは `start` に渡した関数に届く。
    /// 処理中に届いた依頼のうち、答えが返るのは最後の 1 つだけなので、英文が変わるたびに頼んでよい。
    pub fn request(&self, id: u64, text: &str) {
        *self.asked.lock().expect("asked lock") = Some(Asked {
            id,
            text: text.to_string(),
            at: Instant::now(),
            waiting: true,
        });
        // 書き込むスレッドが終わっていたら、補助プロセスも終わっている。`Event::Unavailable` で知らせてある。
        let _ = self.requests.lock().expect("requests lock").send((id, text.to_string()));
    }
}

impl Drop for Reviewer {
    fn drop(&mut self) {
        let mut child = self.child.lock().expect("child lock");
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// 依頼を補助プロセスに書く。補助プロセスが読まなくなると書き込みは止まるが、止まるのはこのスレッドだけ。
fn write_requests(mut stdin: ChildStdin, queue: Receiver<(u64, String)>) {
    while let Ok(mut request) = queue.recv() {
        // 溜まっている依頼は、最後の 1 つだけを書く。
        while let Ok(newer) = queue.try_recv() {
            request = newer;
        }
        let (id, text) = &request;
        let mut line = serde_json::to_string(&Request { id: *id, text }).expect("a request serializes");
        line.push('\n');
        if stdin.write_all(line.as_bytes()).and_then(|()| stdin.flush()).is_err() {
            return;
        }
    }
}

/// 補助プロセスの出力を読む。最初の行は言語モデルが使えるかどうかで、その後は依頼への答え。
fn read_answers(stdout: impl BufRead, asked: &Mutex<Option<Asked>>, available: &AtomicBool, on_event: &dyn Fn(Event)) {
    let mut lines = stdout.lines().map_while(Result::ok);
    let hello: Option<Hello> = lines.next().and_then(|line| serde_json::from_str(&line).ok());
    match hello {
        Some(hello) if hello.available => available.store(true, Ordering::Relaxed),
        Some(hello) => return on_event(Event::Unavailable(hello.reason)),
        None => return on_event(Event::Unavailable("the helper did not start".to_string())),
    }
    for line in lines {
        let event = {
            let mut asked = asked.lock().expect("asked lock");
            let current = asked.as_ref().filter(|asked| asked.waiting).map(|asked| (asked.id, asked.text.clone()));
            let event = parse_answer(&line, current.as_ref());
            if let (Some(_), Some(asked)) = (&event, asked.as_mut()) {
                asked.waiting = false;
            }
            event
        };
        if let Some(event) = event {
            on_event(event);
        }
    }
    // 行が尽きたのは、補助プロセスが終わったから。
    available.store(false, Ordering::Relaxed);
    on_event(Event::Unavailable("the helper stopped".to_string()));
}

/// 答えを待ちすぎた依頼を、答えられなかったと知らせる。補助プロセスが生きたまま止まった時に、
/// 受け取る側が待ち続けないようにする。`Reviewer` が無くなったら終わる。
fn watch_answers(asked: &Weak<Mutex<Option<Asked>>>, timeout: Duration, on_event: &dyn Fn(Event)) {
    loop {
        thread::sleep(timeout / 4);
        let Some(asked) = asked.upgrade() else {
            return;
        };
        let expired = {
            let mut asked = asked.lock().expect("asked lock");
            asked.as_mut().filter(|asked| asked.waiting && asked.at.elapsed() >= timeout).map(|asked| {
                asked.waiting = false;
                asked.id
            })
        };
        if let Some(id) = expired {
            on_event(Event::Failed { id });
        }
    }
}

/// 補助プロセスの 1 行を読む。`asked` は、最後に頼んだ依頼の番号と英文。
///
/// 読めない行と、最後の依頼より前の依頼への答えは捨てる。前の依頼への答えは、打鍵が進む前の
/// 英文に対するもの。
fn parse_answer(line: &str, asked: Option<&(u64, String)>) -> Option<Event> {
    let answer: Answer = serde_json::from_str(line).ok()?;
    let (id, text) = asked?;
    if answer.id != *id {
        return None;
    }
    if answer.error.is_some() || answer.corrected.is_empty() {
        return Some(Event::Failed { id: answer.id });
    }
    Some(Event::Review(Review {
        id: answer.id,
        unchanged: is_unchanged(text, &answer.corrected),
        corrected: answer.corrected,
        translation: answer.translation,
    }))
}

/// 訂正文が、頼んだ英文と実質的に同じかどうか。同じなら、英文は直す所が無い。
/// 前後の空白と、文末に足された句点の違いは無視する。
pub fn is_unchanged(text: &str, corrected: &str) -> bool {
    let normalize = |text: &str| text.trim().trim_end_matches(['.', '!', '?']).trim_end().to_string();
    normalize(text) == normalize(corrected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::mpsc;
    use std::time::Duration;

    /// 補助プロセスの代わりに動かすシェルスクリプトを作る。
    fn helper(name: &str, script: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("kayoiji-check-{}-{name}.sh", std::process::id()));
        std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).expect("script");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("executable");
        path
    }

    /// 補助プロセスからの知らせを、順に受け取れるようにして起動する。
    fn started(name: &str, script: &str) -> (Reviewer, mpsc::Receiver<Event>) {
        let (sender, receiver) = mpsc::channel();
        let reviewer = Reviewer::start(&helper(name, script), move |event| {
            let _ = sender.send(event);
        })
        .expect("starts");
        (reviewer, receiver)
    }

    const WAIT: Duration = Duration::from_secs(5);

    #[test]
    fn an_answer_arrives_for_a_request() {
        // Arrange: 依頼を 1 行読んだら、決まった答えを返す
        let script = r#"echo '{"available":true}'
read line
echo '{"id":7,"corrected":"I have a pen.","translation":"私はペンを持っています。"}'
read line"#;
        let (reviewer, events) = started("answer", script);

        // Act
        reviewer.request(7, "I has a pen");

        // Assert
        let Event::Review(review) = events.recv_timeout(WAIT).expect("an answer") else {
            panic!("a review is expected");
        };
        assert_eq!(review.id, 7);
        assert_eq!(review.corrected, "I have a pen.");
        assert!(!review.unchanged);
        assert_eq!(review.translation, "私はペンを持っています。");
        assert!(reviewer.is_available());
    }

    /// 実際の補助プロセスと言語モデルを使う。Apple Intelligence が有効な Mac で、`pnpm helper` の後に
    /// `KAYOIJI_ENGLISH_CHECK="$PWD/data/build/english-check-<ターゲット名>" cargo test -p kayoiji-check -- --ignored`
    /// で実行する。テストの作業ディレクトリはクレートの場所なので、パスは絶対パスで渡す。
    #[test]
    #[ignore = "needs the helper binary and the on-device language model"]
    fn the_real_helper_corrects_and_translates() {
        // Arrange
        let helper = std::env::var_os("KAYOIJI_ENGLISH_CHECK").expect("the path of the helper");
        let (sender, receiver) = mpsc::channel();
        let reviewer = Reviewer::start(Path::new(&helper), move |event| {
            let _ = sender.send(event);
        })
        .expect("the helper starts");

        // Act
        reviewer.request(1, "I has a pen and two book");

        // Assert
        let Event::Review(review) = receiver.recv_timeout(Duration::from_secs(30)).expect("an answer") else {
            panic!("a review is expected");
        };
        assert!(review.corrected.contains("I have"), "{review:?}");
        assert!(!review.unchanged);
        assert!(!review.translation.is_empty());
    }

    #[test]
    fn an_unavailable_model_is_reported_with_its_reason() {
        // Arrange
        let script = r#"echo '{"available":false,"reason":"appleIntelligenceNotEnabled"}'"#;

        // Act
        let (reviewer, events) = started("unavailable", script);

        // Assert
        let event = events.recv_timeout(WAIT).expect("a report");
        assert_eq!(event, Event::Unavailable("appleIntelligenceNotEnabled".to_string()));
        assert!(!reviewer.is_available());
    }

    #[test]
    fn a_helper_that_stops_is_reported() {
        // Arrange: 使えると知らせた後、すぐに終わる
        let script = r#"echo '{"available":true}'"#;

        // Act
        let (reviewer, events) = started("stops", script);

        // Assert
        assert_eq!(events.recv_timeout(WAIT).expect("a report"), Event::Unavailable("the helper stopped".to_string()));
        assert!(!reviewer.is_available());
    }

    #[test]
    fn a_request_without_an_answer_fails_after_the_timeout() {
        // Arrange: 依頼を読むが、答えない補助プロセス
        let script = "echo '{\"available\":true}'\nread line\nsleep 30";
        let (sender, events) = mpsc::channel();
        let timeout = Duration::from_millis(200);
        let reviewer = Reviewer::start_with_timeout(&helper("silent", script), timeout, move |event| {
            let _ = sender.send(event);
        })
        .expect("starts");

        // Act
        reviewer.request(3, "I has a pen");

        // Assert
        assert_eq!(events.recv_timeout(WAIT).expect("a report"), Event::Failed { id: 3 });
        // 知らせるのは 1 度だけ。
        assert!(events.recv_timeout(timeout * 2).is_err());
    }

    #[test]
    fn an_answered_request_does_not_fail_after_the_timeout() {
        // Arrange
        let script = r#"echo '{"available":true}'
read line
echo '{"id":4,"corrected":"I have a pen.","translation":"私はペンを持っています。"}'
sleep 30"#;
        let (sender, events) = mpsc::channel();
        let timeout = Duration::from_millis(200);
        let reviewer = Reviewer::start_with_timeout(&helper("answered", script), timeout, move |event| {
            let _ = sender.send(event);
        })
        .expect("starts");

        // Act
        reviewer.request(4, "I has a pen");

        // Assert
        assert!(matches!(events.recv_timeout(WAIT), Ok(Event::Review(_))));
        assert!(events.recv_timeout(timeout * 2).is_err());
    }

    #[test]
    fn answers_after_a_timeout_are_dropped() {
        for answer in [
            r#"{"id":9,"corrected":"I have a pen."}"#,
            r#"{"id":9,"error":"late failure"}"#,
        ] {
            // Arrange
            let asked = Mutex::new(Some(Asked {
                id: 9,
                text: "I has a pen".to_string(),
                at: Instant::now(),
                waiting: false,
            }));
            let available = AtomicBool::new(false);
            let input = format!("{{\"available\":true}}\n{answer}\n");
            let (sender, events) = mpsc::channel();

            // Act
            read_answers(input.as_bytes(), &asked, &available, &|event| {
                sender.send(event).unwrap();
            });

            // Assert: EOF の知らせだけが届き、期限切れの依頼の結果は届かない。
            assert_eq!(
                events.try_iter().collect::<Vec<_>>(),
                vec![Event::Unavailable("the helper stopped".to_string())]
            );
        }
    }

    #[test]
    fn a_request_returns_at_once_when_the_helper_does_not_read() {
        // Arrange: 依頼を読まずに待ち続ける補助プロセス
        let (reviewer, _events) = started("stuck", "echo '{\"available\":true}'\nsleep 30");
        let large = "word ".repeat(40_000);

        // Act: パイプの容量を超える量を頼む。書き込みを待つと、ここで止まる
        let started = std::time::Instant::now();
        for id in 0..8 {
            reviewer.request(id, &large);
        }

        // Assert
        assert!(started.elapsed() < Duration::from_secs(2), "{:?}", started.elapsed());
    }

    #[test]
    fn a_missing_helper_is_an_error() {
        let result = Reviewer::start(Path::new("/nonexistent/kayoiji-english-check"), |_| {});
        assert!(result.is_err());
    }

    #[test]
    fn a_failed_answer_is_reported_and_an_unreadable_line_is_dropped() {
        let asked = (1, "I has a pen".to_string());
        let failed = Some(Event::Failed { id: 1 });
        assert_eq!(parse_answer(r#"{"id":1,"error":"the model refused"}"#, Some(&asked)), failed);
        assert_eq!(parse_answer(r#"{"id":1,"corrected":"","translation":""}"#, Some(&asked)), failed);
        assert_eq!(parse_answer("not json", Some(&asked)), None);
    }

    #[test]
    fn an_answer_to_an_earlier_request_is_dropped() {
        // Arrange: 2 番の依頼を出した後に、1 番への答えが届く
        let asked = (2, "I has a pen and".to_string());
        let late = r#"{"id":1,"corrected":"I have a pen.","translation":"私はペンを持っています。"}"#;
        let current = r#"{"id":2,"corrected":"I have a pen and","translation":"私はペンと"}"#;

        // Act, Assert
        assert_eq!(parse_answer(late, Some(&asked)), None);
        assert!(matches!(parse_answer(current, Some(&asked)), Some(Event::Review(review)) if !review.unchanged));
    }

    #[test]
    fn a_correction_that_only_adds_the_final_period_is_no_change() {
        assert!(is_unchanged("Thank you for your quick reply", "Thank you for your quick reply."));
        assert!(is_unchanged(" What is your name? ", "What is your name?"));
        assert!(!is_unchanged("I has a pen", "I have a pen."));
    }
}
