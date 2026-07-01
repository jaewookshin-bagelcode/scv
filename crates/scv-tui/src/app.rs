//! 대화형 터미널 앱 — 에이전트 루프와 ratatui 렌더/입력 루프를 한 태스크에서 **동시에**
//! 굴린다(ARCHITECTURE §4.5). 둘은 직접 호출이 아니라 채널로 통신한다:
//! - 루프 → UI: [`AgentEvent`] (관찰 전용, [`ChannelObserver`])
//! - UI → 루프: 권한 결정([`InteractivePermissionGate`] 의 `decide` 반환값) + 취소
//!   ([`CancellationToken`])
//!
//! 한 태스크 안에서 `run_turn` 미래를 `select!` 의 한 갈래로 폴링하므로, 도구 승인 대기처럼
//! `run_turn` 이 await 로 멈춘 동안에도 입력·렌더·승인 갈래가 계속 돈다. spawn 이 없어
//! `&mut session`/`&agent` 빌림이 그대로 살아 있다(Send/'static 제약 없음).

use std::io::{self, Stdout};
use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use futures::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::{Frame, Terminal};
use scv_core::agent::Agent;
use scv_core::message::{AgentEvent, StopReason, StreamEvent};
use scv_core::provider::{ModelInfo, Provider};
use scv_core::session::{Session, SessionStore};
use scv_core::skill::SkillRegistry;
use scv_core::tool::{CancellationToken, PermissionLevel};

/// 합성 루트(scv-cli)가 주입하는 "프로바이더 빌드 팩토리". 프로바이더 id 를 받아 그
/// 프로바이더와 기본 모델을 만든다. scv-tui 는 설정/`scv_providers` 를 모르므로(코어만 의존),
/// 실제 빌드는 이 클로저가 한다(`/provider` 명령이 호출).
pub type MakeProvider<'a> = dyn Fn(&str) -> scv_core::Result<(Arc<dyn Provider>, String)> + 'a;

use crate::format_tool_output_for_display;
use crate::observer::ChannelObserver;
use crate::permission::{InteractivePermissionGate, PermissionRequest};
use crate::phase::{Phase, SpinnerStyle};

type Term = Terminal<CrosstermBackend<Stdout>>;

/// PageUp/PageDown 한 번에 스크롤하는 줄 수.
const SCROLL_STEP: u16 = 5;

/// crossterm/ratatui IO 오류를 core 에러로 감싼다(라이브러리 경계, CODING_RULES §2).
fn map_io(context: &str, source: io::Error) -> scv_core::Error {
    scv_core::Error::Io {
        context: context.to_string(),
        source,
    }
}

/// raw mode + 대체 화면 진입을 `Drop` 으로 감싸 정상 종료·취소·**패닉** 어느 경우에도
/// 터미널을 복원한다(§4.5).
struct RawModeGuard;

impl RawModeGuard {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        execute!(io::stdout(), EnterAlternateScreen)?;
        Ok(Self)
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        let _ = disable_raw_mode();
    }
}

/// 키 입력의 의미(순수 분류). raw mode 에선 Ctrl-C 가 SIGINT 가 아니라 키 이벤트로 온다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Key {
    Insert(char),
    Backspace,
    Submit,
    /// Ctrl-C.
    Interrupt,
    /// Esc.
    Cancel,
    /// PageUp — 대화 로그를 위로 스크롤(이전 대화 보기).
    ScrollUp,
    /// PageDown — 아래로 스크롤(하단 추적 복귀).
    ScrollDown,
    Ignore,
}

/// crossterm 키 이벤트를 의미로 분류한다 — **순수**(테스트 가능).
pub(crate) fn decode_key(key: KeyEvent) -> Key {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Char('c') if ctrl => Key::Interrupt,
        KeyCode::PageUp => Key::ScrollUp,
        KeyCode::PageDown => Key::ScrollDown,
        KeyCode::Char(c) if !ctrl => Key::Insert(c),
        KeyCode::Enter => Key::Submit,
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Esc => Key::Cancel,
        _ => Key::Ignore,
    }
}

/// 슬래시 명령. 입력이 `/` 로 시작하면 일반 프롬프트 대신 이걸로 해석한다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Command {
    /// 프로바이더 전환(그 프로바이더의 기본 모델로).
    Provider(String),
    /// 현재 프로바이더에서 모델만 전환.
    Model(String),
    /// 현재 프로바이더에서 사용 가능한 모델 목록.
    Models,
    /// 사용 가능한 프로바이더 목록.
    Providers,
    /// 사용 가능한 스킬 목록.
    Skills,
    /// 도움말.
    Help,
    /// 알 수 없는 명령 — 스킬 이름일 수 있어(`/<skill>`) 루프에서 한 번 더 확인한다.
    Unknown(String),
}

/// 입력이 슬래시 명령이면 파싱한다(아니면 `None` → 일반 프롬프트로 처리) — **순수**.
pub(crate) fn parse_command(line: &str) -> Option<Command> {
    let rest = line.trim().strip_prefix('/')?;
    let mut it = rest.split_whitespace();
    let cmd = it.next().unwrap_or("");
    let arg = it.next().unwrap_or("");
    Some(match (cmd, arg) {
        ("provider" | "p", a) if !a.is_empty() => Command::Provider(a.to_string()),
        ("model" | "m", a) if !a.is_empty() => Command::Model(a.to_string()),
        ("models", _) => Command::Models,
        ("providers", _) => Command::Providers,
        ("skills", _) => Command::Skills,
        ("help" | "h" | "?", _) => Command::Help,
        (other, _) => Command::Unknown(other.to_string()),
    })
}

/// 진행 중인 승인 모달.
struct Modal {
    tool: String,
    /// 무엇을 승인하는지 한 줄 요약(bash 명령·대상 경로 등). 사용자가 블라인드 승인하지
    /// 않도록 보여준다(§4.3: bash 명령은 신뢰 불가 입력).
    summary: String,
    reply: tokio::sync::oneshot::Sender<PermissionLevel>,
}

/// 사용자 입력 수집 결과.
#[derive(Debug)]
enum Prompt {
    Submit(String),
    Quit,
}

/// 대화형 앱 상태. 렌더는 이 상태만 읽는다(`&self`).
pub struct App {
    spinner: SpinnerStyle,
    color: bool,
    phase: Phase,
    spinner_tick: usize,
    input: String,
    transcript: Vec<String>,
    /// 현재 스트리밍 중인 어시스턴트 텍스트(완성되면 transcript 로 flush).
    live: String,
    /// 현재 스트리밍 중인 사고(thinking) — 흐리게 실시간 표시, transcript 엔 보존하지 않음
    /// (휘발성). 답 텍스트가 나오거나 메시지가 끝나면 비운다.
    live_thinking: String,
    modal: Option<Modal>,
    /// idle 에서 Ctrl-C 한 번 누른 상태(더블 프레스로 종료).
    quit_armed: bool,
    hint: String,
    /// 현재 프로바이더·모델 라벨(입력창 제목에 표시). `/provider`·`/model` 로 갱신.
    model_label: String,
    /// 현재 프로바이더의 실시간 모델 카탈로그 캐시(`/models` 표시·`/model` 검증용). 시작 시와
    /// `/provider` 전환 후 `Provider::list_models` 로 갱신한다. 조회 실패면 비어 있고, 그 경우
    /// `/model` 은 검증을 건너뛰고 무엇이든 허용한다.
    models: Vec<ModelInfo>,
    /// 대화 로그를 하단에서 위로 끌어올린 줄 수(0 = 하단 추적). PageUp/Down 으로 조절.
    scroll_lines: u16,
}

impl std::fmt::Debug for App {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("App")
            .field("spinner", &self.spinner)
            .field("phase", &self.phase)
            .field("transcript_lines", &self.transcript.len())
            .finish_non_exhaustive()
    }
}

impl App {
    pub fn new(spinner: SpinnerStyle) -> Self {
        Self {
            spinner,
            color: std::env::var_os("NO_COLOR").is_none(),
            phase: Phase::Idle,
            spinner_tick: 0,
            input: String::new(),
            transcript: Vec::new(),
            live: String::new(),
            live_thinking: String::new(),
            modal: None,
            quit_armed: false,
            hint: String::new(),
            model_label: String::new(),
            models: Vec::new(),
            scroll_lines: 0,
        }
    }

    /// 대화 루프. 입력을 받아 턴을 구동하고, 권한 모달·인터럽트·진행 표시를 처리한다.
    /// `agent.permissions` 를 대화형 게이트로 감싸고, 턴마다 새 취소 토큰을 주입한다.
    pub async fn run(
        &mut self,
        mut agent: Agent,
        mut session: Session,
        store: &dyn SessionStore,
        providers: &[String],
        provider_id: &str,
        make_provider: &MakeProvider<'_>,
        skills: &SkillRegistry,
    ) -> scv_core::Result<()> {
        // 라벨은 **설정 프로바이더 id**(예: aiproxy)를 쓴다. `agent.provider.id()` 는 어댑터
        // 와이어 종류(anthropic)라 aiproxy 처럼 한 어댑터가 여러 설정 id 를 서빙하면 어긋난다
        // (`/provider` 전환 라벨과 동일한 기준). 모델은 실행 중 `/model` 로 바뀔 수 있다.
        self.model_label = format!("{provider_id}·{}", agent.model);
        // 실시간 모델 카탈로그를 캐시(aiproxy 는 GET /api/v1/models/anthropic 조회). 실패하면
        // 빈 목록 → `/model` 검증은 건너뛰고 `/models` 는 "없음" 을 표시(시작을 막지 않음).
        self.models = agent.provider.list_models().await.unwrap_or_default();
        let guard = RawModeGuard::enter().map_err(|e| map_io("enter raw mode", e))?;
        let backend = CrosstermBackend::new(io::stdout());
        let mut terminal = Terminal::new(backend).map_err(|e| map_io("init terminal", e))?;
        let mut input = EventStream::new();
        let mut tick = tokio::time::interval(Duration::from_millis(80));

        // 대화형 게이트 배선(턴 간 유지): 정적 정책 + 대화형 프롬프트 합성.
        let (perm_tx, mut perm_rx) = tokio::sync::mpsc::channel::<PermissionRequest>(4);
        let static_gate = agent.permissions.clone();
        agent.permissions = Arc::new(InteractivePermissionGate::new(static_gate, perm_tx));

        // 이벤트 채널(턴 간 유지). observer 는 매 턴 run_turn 에 &로 넘긴다.
        let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();
        let observer = ChannelObserver::new(event_tx);

        // 시작 안내: 현재 모델 + 사용 가능한 스킬 + 도움말을 한 줄 띄운다(스킬이 있는데도
        // /skills 를 쳐야만 보여서 "없는 것처럼" 보이던 문제 해소).
        let skill_names: Vec<&str> = skills.summaries().map(|m| m.name.as_str()).collect();
        let skills_note = if skill_names.is_empty() {
            "no skills".to_string()
        } else {
            format!("skills: {} (run with /<name>)", skill_names.join(", "))
        };
        self.transcript.push(format!(
            "scv · {} · {skills_note} · /help",
            self.model_label
        ));
        self.hint = "type a message · enter to send · ctrl-c to quit".into();
        self.render(&mut terminal)?;

        loop {
            // ── IDLE: 프롬프트 수집 ──
            let prompt = match self
                .read_prompt(&mut input, &mut tick, &mut terminal)
                .await?
            {
                Prompt::Submit(p) => p,
                Prompt::Quit => break,
            };
            let raw = prompt;
            if raw.trim().is_empty() {
                continue;
            }
            // 입력 분류 → 이번 턴에 모델로 보낼 prompt 결정.
            //  - 슬래시 명령(/provider, /model, /providers, /skills, /help): 즉시 처리 후 continue.
            //  - `/<스킬이름>`: 그 스킬 본문을 지시로 주입해 이번 턴 실행(progressive disclosure 발동).
            //  - 그 외: 일반 프롬프트.
            let prompt: String = if let Some(cmd) = parse_command(&raw) {
                match cmd {
                    Command::Unknown(ref name) if skills.get(name).is_some() => {
                        let skill = skills.get(name).expect("just checked");
                        self.transcript.push(format!("› {raw}"));
                        self.transcript.push(format!("[skill: {name}]"));
                        let body = skill.body.clone().unwrap_or_default();
                        format!("\"{name}\" 스킬의 절차를 따라 진행한다:\n\n{body}")
                    }
                    other => {
                        self.transcript.push(format!("› {raw}"));
                        let switched = self.handle_command(
                            other,
                            &mut agent,
                            providers,
                            make_provider,
                            skills,
                        );
                        // 프로바이더가 바뀌었으면 새 프로바이더의 카탈로그로 캐시를 갱신한다.
                        if switched {
                            self.models = agent.provider.list_models().await.unwrap_or_default();
                        }
                        self.render(&mut terminal)?;
                        continue;
                    }
                }
            } else {
                self.transcript.push(format!("› {raw}"));
                raw
            };

            // ── RUNNING: 턴 구동 ──
            let token = CancellationToken::new();
            agent.tool_ctx.cancel = token.clone();
            self.scroll_lines = 0; // 새 턴이면 하단 추적으로 복귀.
            self.phase = Phase::Waiting;
            self.live.clear();
            self.live_thinking.clear();
            self.quit_armed = false;
            self.hint = "ctrl-c to interrupt".into();
            self.render(&mut terminal)?;

            let outcome = {
                let turn = agent.run_turn(&mut session, prompt, &observer);
                tokio::pin!(turn);
                loop {
                    tokio::select! {
                        biased;
                        res = &mut turn => break res,
                        Some(ev) = event_rx.recv() => {
                            self.apply_event(&ev);
                            self.render(&mut terminal)?;
                        }
                        Some(req) = perm_rx.recv() => {
                            self.open_modal(req);
                            self.render(&mut terminal)?;
                        }
                        maybe = input.next() => {
                            if let Some(Ok(Event::Key(key))) = maybe {
                                self.handle_running_key(key, &token);
                                self.render(&mut terminal)?;
                            }
                        }
                        _ = tick.tick() => {
                            if self.phase.is_active() {
                                self.spinner_tick = self.spinner_tick.wrapping_add(1);
                                self.render(&mut terminal)?;
                            }
                        }
                    }
                }
            };

            // 턴 종료 후 남은 이벤트 드레인(마지막 텍스트/스톱 반영).
            while let Ok(ev) = event_rx.try_recv() {
                self.apply_event(&ev);
            }
            // 모달이 떠있던 채로 끝났다면(이론상 드묾) 정리 — 보낸 적 없으면 게이트가 fail-closed.
            self.modal = None;
            self.flush_live(StopReason::EndTurn);

            match outcome {
                Ok(()) => {}
                Err(scv_core::Error::Cancelled) => {
                    self.transcript.push("[interrupted — partial saved]".into());
                }
                Err(scv_core::Error::PermissionDenied(tool)) => {
                    self.transcript
                        .push(format!("[denied: {tool} — turn aborted]"));
                }
                Err(e) => self.transcript.push(format!("[error: {e}]")),
            }
            self.phase = Phase::Idle;
            self.hint = "type a message · enter to send · ctrl-c to quit".into();

            // 세션 저장(턴마다 영속화 → 재개 가능).
            if let Err(e) = store.save(&session).await {
                self.transcript.push(format!("[session save failed: {e}]"));
            }
            self.render(&mut terminal)?;
        }

        // 터미널을 복원(대체화면 나가기·raw 해제)한 뒤 일반 화면에 세션 id 를 알린다 →
        // `scv --resume <id>` 로 이어갈 수 있다. 턴이 한 번도 없었으면(저장 안 됨) 생략.
        drop(terminal);
        drop(guard);
        if !session.messages.is_empty() {
            println!(
                "[session {id}]  (resume: scv --resume {id})",
                id = session.id.0
            );
        }
        Ok(())
    }

    /// idle 상태에서 한 줄 입력을 모은다. Enter=제출, 더블 Ctrl-C=종료.
    async fn read_prompt(
        &mut self,
        input: &mut EventStream,
        tick: &mut tokio::time::Interval,
        terminal: &mut Term,
    ) -> scv_core::Result<Prompt> {
        loop {
            tokio::select! {
                maybe = input.next() => match maybe {
                    Some(Ok(Event::Key(key))) => {
                        if let Some(p) = self.handle_idle_key(key) {
                            return Ok(p);
                        }
                        self.render(terminal)?;
                    }
                    Some(Ok(_)) => {}                 // resize 등은 다음 렌더에서 반영.
                    Some(Err(e)) => return Err(map_io("read input", e)),
                    None => return Ok(Prompt::Quit),  // 입력 스트림 종료.
                },
                _ = tick.tick() => {}                 // idle 엔 스피너 없음 — 렌더 생략.
            }
        }
    }

    /// idle 키 처리. 제출/종료면 `Some`, 계속 입력이면 `None`.
    fn handle_idle_key(&mut self, key: KeyEvent) -> Option<Prompt> {
        match decode_key(key) {
            Key::Interrupt => {
                if self.quit_armed {
                    return Some(Prompt::Quit);
                }
                self.quit_armed = true;
                self.hint = "press ctrl-c again to quit".into();
                None
            }
            Key::Submit => {
                let p = std::mem::take(&mut self.input);
                self.quit_armed = false;
                Some(Prompt::Submit(p))
            }
            Key::Backspace => {
                self.input.pop();
                self.quit_armed = false;
                None
            }
            Key::Insert(c) => {
                self.input.push(c);
                self.quit_armed = false;
                None
            }
            Key::Cancel => {
                self.input.clear();
                self.quit_armed = false;
                None
            }
            Key::ScrollUp => {
                self.scroll_lines = self.scroll_lines.saturating_add(SCROLL_STEP);
                None
            }
            Key::ScrollDown => {
                self.scroll_lines = self.scroll_lines.saturating_sub(SCROLL_STEP);
                None
            }
            Key::Ignore => None,
        }
    }

    /// 턴 진행 중 키 처리. 모달이 있으면 y/n 으로 승인 결정, 없으면 Ctrl-C 로 턴 취소.
    /// 스크롤은 진행 중에도 가능.
    fn handle_running_key(&mut self, key: KeyEvent, token: &CancellationToken) {
        let decoded = decode_key(key);
        if self.modal.is_some() {
            match decoded {
                Key::Insert('y') | Key::Insert('Y') | Key::Submit => {
                    self.resolve_modal(PermissionLevel::Allow)
                }
                Key::Insert('n') | Key::Insert('N') | Key::Cancel => {
                    self.resolve_modal(PermissionLevel::Deny)
                }
                _ => {}
            }
            return;
        }
        match decoded {
            Key::ScrollUp => self.scroll_lines = self.scroll_lines.saturating_add(SCROLL_STEP),
            Key::ScrollDown => self.scroll_lines = self.scroll_lines.saturating_sub(SCROLL_STEP),
            Key::Interrupt => {
                token.cancel();
                self.hint = "interrupting…".into();
            }
            _ => {}
        }
    }

    fn open_modal(&mut self, req: PermissionRequest) {
        self.phase = Phase::AwaitingPermission(req.tool.clone());
        let summary = summarize_input(&req.tool, &req.input);
        self.modal = Some(Modal {
            tool: req.tool,
            summary,
            reply: req.reply,
        });
    }

    fn resolve_modal(&mut self, level: PermissionLevel) {
        if let Some(modal) = self.modal.take() {
            let verb = if level == PermissionLevel::Allow {
                "approved"
            } else {
                "denied"
            };
            self.transcript.push(format!("[{verb}: {}]", modal.tool));
            // 게이트가 응답을 기다린다. 드롭됐어도(fail-closed) 무시.
            let _ = modal.reply.send(level);
        }
    }

    /// 루프 통지를 화면 상태로 반영한다.
    fn apply_event(&mut self, event: &AgentEvent) {
        self.phase = self.phase.next(event);
        match event {
            AgentEvent::Stream(StreamEvent::TextDelta(t)) => self.live.push_str(t),
            AgentEvent::Stream(StreamEvent::ThinkingDelta(t)) => self.live_thinking.push_str(t),
            AgentEvent::Stream(StreamEvent::MessageStop { stop_reason, .. }) => {
                self.flush_live(*stop_reason)
            }
            AgentEvent::ToolStart { name } => self.transcript.push(format!("⚙ {name}")),
            AgentEvent::ToolEnd {
                name,
                content,
                is_error,
            } => {
                if *is_error {
                    self.transcript.push(format!("✗ {name} failed"));
                }
                self.push_tool_output(name, content);
            }
            _ => {}
        }
    }

    /// 슬래시 명령 처리(프로바이더/모델 전환 등). 결과를 transcript 에 한 줄로 남긴다.
    /// 프로바이더 전환은 주입된 `make_provider` 가 실제 빌드를 한다(scv-cli).
    /// **반환값**: 프로바이더가 바뀌었는가(true 면 호출자가 모델 카탈로그 캐시를 갱신).
    fn handle_command(
        &mut self,
        cmd: Command,
        agent: &mut Agent,
        providers: &[String],
        make_provider: &MakeProvider<'_>,
        skills: &SkillRegistry,
    ) -> bool {
        match cmd {
            Command::Provider(id) => match make_provider(&id) {
                Ok((provider, model)) => {
                    agent.provider = provider;
                    agent.model = model.clone();
                    self.model_label = format!("{id}·{model}");
                    self.transcript.push(format!("[switched: {id} · {model}]"));
                    true // 프로바이더 교체됨 → 카탈로그 갱신 필요.
                }
                Err(e) => {
                    self.transcript
                        .push(format!("[provider switch failed: {e}]"));
                    false
                }
            },
            Command::Model(m) => {
                // 실시간 카탈로그(self.models)에 대해 검증한다. 오타·잘못된 ID 가 조용히 통과해
                // 나중에 API 404 로 터지는 것을 막는다(`/provider` 실패 처리와 대칭). 카탈로그가
                // 비면(조회 실패·미지원) 검증할 수 없으므로 그대로 허용한다.
                let valid = self.models.is_empty() || self.models.iter().any(|mi| mi.id == m);
                if valid {
                    agent.model = m.clone();
                    self.model_label = format!("{}·{m}", agent.provider.id());
                    self.transcript.push(format!("[switched model: {m}]"));
                } else {
                    let ids = self
                        .models
                        .iter()
                        .map(|mi| mi.id.as_str())
                        .collect::<Vec<_>>()
                        .join(", ");
                    let pid = agent.provider.id().to_string();
                    self.transcript.push(format!(
                        "[unknown model: {m} — '{pid}' 에서 사용 가능: {ids}]"
                    ));
                }
                false
            }
            Command::Models => {
                let cur = agent.model.as_str();
                let list: Vec<String> = self
                    .models
                    .iter()
                    .map(|m| {
                        if m.id == cur {
                            format!("{} (현재)", m.id)
                        } else {
                            m.id.clone()
                        }
                    })
                    .collect();
                if list.is_empty() {
                    let pid = agent.provider.id();
                    self.transcript
                        .push(format!("[models: '{pid}' 프로바이더에 등록된 모델이 없음]"));
                } else {
                    self.transcript
                        .push(format!("[models: {}]", list.join(", ")));
                }
                false
            }
            Command::Providers => {
                self.transcript
                    .push(format!("[providers: {}]", providers.join(", ")));
                false
            }
            Command::Skills => {
                let names: Vec<&str> = skills.summaries().map(|m| m.name.as_str()).collect();
                self.transcript
                    .push(format!("[skills: {} — run with /<name>]", names.join(", ")));
                false
            }
            Command::Help => {
                self.transcript.push(
                    "[commands] /provider <id> · /model <id> · /providers · /models · /skills · /<skill> · /help"
                        .to_string(),
                );
                self.transcript.push(
                    "[keys] PageUp / PageDown 키로 대화 스크롤 · Ctrl-C 중단/종료".to_string(),
                );
                false
            }
            Command::Unknown(c) => {
                self.transcript
                    .push(format!("[unknown: /{c} — try /help or /skills]"));
                false
            }
        }
    }

    fn push_tool_output(&mut self, name: &str, content: &str) {
        let Some(output) = format_tool_output_for_display(content) else {
            return;
        };
        self.transcript.push(format!("[{name} output]"));
        self.transcript
            .extend(output.lines().map(std::string::ToString::to_string));
    }

    /// 누적된 스트리밍 텍스트를 transcript 로 옮긴다(빈 건 버림). 최종 응답이 thinking-only
    /// 로 끝나는 호환 백엔드 응답만 fallback 으로 보여주고, tool-use 중간 사고는 비운다.
    fn flush_live(&mut self, stop_reason: StopReason) {
        let thinking = std::mem::take(&mut self.live_thinking);
        let text = std::mem::take(&mut self.live);
        let trimmed = text.trim_end();
        if !trimmed.is_empty() {
            self.transcript.push(trimmed.to_string());
            return;
        }
        if stop_reason == StopReason::EndTurn {
            let trimmed = thinking.trim_end();
            if !trimmed.is_empty() {
                self.transcript.push(trimmed.to_string());
            }
        }
    }

    fn render(&self, terminal: &mut Term) -> scv_core::Result<()> {
        terminal
            .draw(|f| self.draw(f))
            .map_err(|e| map_io("draw", e))?;
        Ok(())
    }

    fn draw(&self, f: &mut Frame<'_>) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(1),    // transcript
                Constraint::Length(1), // status
                Constraint::Length(3), // input
            ])
            .split(f.area());

        self.draw_transcript(f, chunks[0]);
        self.draw_status(f, chunks[1]);
        self.draw_input(f, chunks[2]);

        if self.modal.is_some() {
            self.draw_modal(f);
        }
    }

    fn draw_transcript(&self, f: &mut Frame<'_>, area: Rect) {
        let block = Block::default().borders(Borders::ALL).title(" scv ");
        let inner = block.inner(area);

        let mut lines: Vec<Line<'_>> = self
            .transcript
            .iter()
            .map(|l| Line::raw(l.clone()))
            .collect();
        // 실시간 스트리밍: 답 텍스트가 시작됐으면 그걸(끝에 캐럿), 아직 사고만 흐르면 사고를
        // 흐리게 보여준다 — 사고를 안 보여주면 긴 reasoning 동안 화면이 빈 것처럼 보인다.
        if !self.live.is_empty() {
            lines.push(Line::from(format!("{}▋", self.live)));
        } else if !self.live_thinking.is_empty() {
            for tl in self.live_thinking.lines() {
                lines.push(Line::from(self.styled(
                    tl.to_string(),
                    Color::DarkGray,
                    true,
                )));
            }
        }

        let para = Paragraph::new(lines).wrap(Wrap { trim: false });
        // 줄바꿈(wrap)까지 고려한 실제 행 수로 스크롤한다. 기본은 하단(최신 응답)이고,
        // scroll_lines 만큼 위로 끌어올려 이전 대화를 본다(PageUp/Down). 끝값은 클램프.
        let total = para.line_count(inner.width) as u16;
        let bottom = total.saturating_sub(inner.height);
        let scroll = bottom.saturating_sub(self.scroll_lines.min(bottom));
        f.render_widget(para.block(block).scroll((scroll, 0)), area);
    }

    fn draw_status(&self, f: &mut Frame<'_>, area: Rect) {
        let mut spans = Vec::new();
        if self.phase.is_active() {
            let glyph = self.spinner.frame(self.spinner_tick);
            spans.push(self.styled(format!("{glyph} "), Color::Cyan, false));
        }
        spans.push(self.styled(self.phase.label(), Color::Gray, false));
        if !self.hint.is_empty() {
            spans.push(Span::raw("   "));
            spans.push(self.styled(self.hint.clone(), Color::DarkGray, true));
        }
        f.render_widget(Paragraph::new(Line::from(spans)), area);
    }

    fn draw_input(&self, f: &mut Frame<'_>, area: Rect) {
        let title = if self.model_label.is_empty() {
            " message ".to_string()
        } else {
            format!(" message — {} (/help) ", self.model_label)
        };
        let para = Paragraph::new(format!("› {}", self.input))
            .block(Block::default().borders(Borders::ALL).title(title));
        f.render_widget(para, area);
    }

    fn draw_modal(&self, f: &mut Frame<'_>) {
        let Some(modal) = &self.modal else { return };
        let area = centered_rect(70, 8, f.area());
        f.render_widget(Clear, area);
        let body = vec![
            Line::raw(""),
            Line::from(self.styled(
                format!("  Tool `{}` wants to run:", modal.tool),
                Color::Yellow,
                false,
            )),
            Line::from(self.styled(format!("    {}", modal.summary), Color::White, false)),
            Line::raw(""),
            Line::from(self.styled("  [y] approve    [n] deny".to_string(), Color::Gray, false)),
        ];
        let para = Paragraph::new(body)
            .alignment(Alignment::Left)
            .wrap(Wrap { trim: false })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" permission required "),
            );
        f.render_widget(para, area);
    }

    /// `NO_COLOR` 를 존중하는 스타일 span 생성.
    fn styled(&self, text: String, color: Color, dim: bool) -> Span<'static> {
        if !self.color {
            return Span::raw(text);
        }
        let mut style = Style::default().fg(color);
        if dim {
            style = style.add_modifier(Modifier::DIM);
        }
        Span::styled(text, style)
    }
}

/// 승인 모달에 보일 입력 한 줄 요약 — **순수**(테스트 가능). 도구별로 가장 의미 있는
/// 필드(bash→command, write/edit/read→path)를 뽑고, 없으면 compact JSON. 길면 자른다.
fn summarize_input(tool: &str, input: &serde_json::Value) -> String {
    let key = match tool {
        "bash" => Some("command"),
        "write" | "edit" | "read" => Some("path"),
        _ => None,
    };
    let raw = key
        .and_then(|k| input.get(k))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| input.to_string());
    truncate_oneline(&raw, 160)
}

/// 한 줄로 만들고(개행→공백) `max` 글자에서 자른다(말줄임).
fn truncate_oneline(s: &str, max: usize) -> String {
    let oneline = s.replace(['\n', '\r'], " ");
    if oneline.chars().count() <= max {
        return oneline;
    }
    let cut: String = oneline.chars().take(max.saturating_sub(1)).collect();
    format!("{cut}…")
}

/// 화면 중앙에 `height` 줄짜리 폭 `percent_x%` 사각형을 만든다.
fn centered_rect(percent_x: u16, height: u16, area: Rect) -> Rect {
    let w = area.width * percent_x / 100;
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    Rect {
        x,
        y,
        width: w,
        height: height.min(area.height),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEventKind;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::empty(),
            kind: KeyEventKind::Press,
            state: crossterm::event::KeyEventState::empty(),
        }
    }

    fn ctrl(code: KeyCode) -> KeyEvent {
        let mut k = key(code);
        k.modifiers = KeyModifiers::CONTROL;
        k
    }

    #[test]
    fn decode_key_classifies_inputs() {
        assert_eq!(decode_key(ctrl(KeyCode::Char('c'))), Key::Interrupt);
        assert_eq!(decode_key(key(KeyCode::Char('a'))), Key::Insert('a'));
        assert_eq!(decode_key(key(KeyCode::Enter)), Key::Submit);
        assert_eq!(decode_key(key(KeyCode::Backspace)), Key::Backspace);
        assert_eq!(decode_key(key(KeyCode::Esc)), Key::Cancel);
        assert_eq!(decode_key(key(KeyCode::Tab)), Key::Ignore);
        // Ctrl 조합 문자는 삽입하지 않는다(Ctrl-C 외엔 Ignore).
        assert_eq!(decode_key(ctrl(KeyCode::Char('a'))), Key::Ignore);
    }

    #[test]
    fn idle_typing_and_submit() {
        let mut app = App::new(SpinnerStyle::Ascii);
        assert!(app.handle_idle_key(key(KeyCode::Char('h'))).is_none());
        assert!(app.handle_idle_key(key(KeyCode::Char('i'))).is_none());
        assert_eq!(app.input, "hi");
        match app.handle_idle_key(key(KeyCode::Enter)) {
            Some(Prompt::Submit(p)) => assert_eq!(p, "hi"),
            other => panic!("expected submit, got {other:?}"),
        }
        // 제출 후 입력 버퍼는 비워진다.
        assert_eq!(app.input, "");
    }

    #[test]
    fn idle_backspace_edits_buffer() {
        let mut app = App::new(SpinnerStyle::Ascii);
        app.handle_idle_key(key(KeyCode::Char('a')));
        app.handle_idle_key(key(KeyCode::Char('b')));
        app.handle_idle_key(key(KeyCode::Backspace));
        assert_eq!(app.input, "a");
    }

    #[test]
    fn pageup_pagedown_scroll_transcript() {
        let mut app = App::new(SpinnerStyle::Ascii);
        assert_eq!(app.scroll_lines, 0);
        app.handle_idle_key(key(KeyCode::PageUp));
        assert_eq!(app.scroll_lines, SCROLL_STEP);
        app.handle_idle_key(key(KeyCode::PageUp));
        assert_eq!(app.scroll_lines, SCROLL_STEP * 2);
        app.handle_idle_key(key(KeyCode::PageDown));
        assert_eq!(app.scroll_lines, SCROLL_STEP);
        // 하단 초과로 내려도 0 에서 멈춘다(saturating).
        app.handle_idle_key(key(KeyCode::PageDown));
        app.handle_idle_key(key(KeyCode::PageDown));
        assert_eq!(app.scroll_lines, 0);
    }

    #[test]
    fn idle_double_ctrl_c_quits() {
        let mut app = App::new(SpinnerStyle::Ascii);
        // 1차: 종료 안내만, 종료 아님.
        assert!(app.handle_idle_key(ctrl(KeyCode::Char('c'))).is_none());
        assert!(app.quit_armed);
        // 2차: 종료.
        assert!(matches!(
            app.handle_idle_key(ctrl(KeyCode::Char('c'))),
            Some(Prompt::Quit)
        ));
    }

    #[test]
    fn typing_disarms_quit() {
        let mut app = App::new(SpinnerStyle::Ascii);
        app.handle_idle_key(ctrl(KeyCode::Char('c')));
        assert!(app.quit_armed);
        app.handle_idle_key(key(KeyCode::Char('x')));
        assert!(!app.quit_armed);
    }

    #[tokio::test]
    async fn modal_y_n_resolve_permission() {
        for (k, expected) in [
            (KeyCode::Char('y'), PermissionLevel::Allow),
            (KeyCode::Char('n'), PermissionLevel::Deny),
        ] {
            let mut app = App::new(SpinnerStyle::Ascii);
            let (tx, rx) = tokio::sync::oneshot::channel();
            app.open_modal(PermissionRequest {
                tool: "bash".into(),
                input: serde_json::json!({}),
                reply: tx,
            });
            assert!(app.modal.is_some());
            let token = CancellationToken::new();
            app.handle_running_key(key(k), &token);
            assert!(app.modal.is_none(), "modal should close after decision");
            assert_eq!(rx.await.unwrap(), expected);
        }
    }

    #[test]
    fn ctrl_c_during_turn_cancels_token() {
        let mut app = App::new(SpinnerStyle::Ascii);
        let token = CancellationToken::new();
        app.handle_running_key(ctrl(KeyCode::Char('c')), &token);
        assert!(token.is_cancelled());
    }

    #[test]
    fn apply_event_streams_text_then_flushes_on_stop() {
        use scv_core::message::{StopReason, Usage};
        let mut app = App::new(SpinnerStyle::Ascii);
        app.apply_event(&AgentEvent::Stream(StreamEvent::TextDelta("hello ".into())));
        app.apply_event(&AgentEvent::Stream(StreamEvent::TextDelta("world".into())));
        assert_eq!(app.live, "hello world");
        app.apply_event(&AgentEvent::Stream(StreamEvent::MessageStop {
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
        }));
        assert_eq!(app.live, "");
        assert_eq!(app.transcript.last().unwrap(), "hello world");
    }

    #[test]
    fn apply_event_flushes_final_thinking_only_response() {
        use scv_core::message::{StopReason, Usage};
        let mut app = App::new(SpinnerStyle::Ascii);
        app.apply_event(&AgentEvent::Stream(StreamEvent::ThinkingDelta(
            "compat final answer".into(),
        )));
        app.apply_event(&AgentEvent::Stream(StreamEvent::MessageStop {
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
        }));
        assert_eq!(app.live_thinking, "");
        assert_eq!(app.transcript.last().unwrap(), "compat final answer");
    }

    #[test]
    fn apply_event_discards_thinking_before_tool_use_stop() {
        use scv_core::message::{StopReason, Usage};
        let mut app = App::new(SpinnerStyle::Ascii);
        app.apply_event(&AgentEvent::Stream(StreamEvent::ThinkingDelta(
            "tool plan".into(),
        )));
        app.apply_event(&AgentEvent::Stream(StreamEvent::MessageStop {
            stop_reason: StopReason::ToolUse,
            usage: Usage::default(),
        }));
        assert_eq!(app.live_thinking, "");
        assert!(app.transcript.is_empty());
    }

    #[test]
    fn apply_event_records_failed_tool() {
        let mut app = App::new(SpinnerStyle::Ascii);
        app.apply_event(&AgentEvent::ToolStart {
            name: "bash".into(),
        });
        app.apply_event(&AgentEvent::ToolEnd {
            name: "bash".into(),
            content: "bad command\n[exit: 127]".into(),
            is_error: true,
        });
        assert!(app.transcript.iter().any(|l| l.contains("bash failed")));
        assert!(app.transcript.iter().any(|l| l.contains("[bash output]")));
        assert!(app.transcript.iter().any(|l| l.contains("bad command")));
    }

    #[test]
    fn summarize_input_picks_meaningful_field() {
        assert_eq!(
            summarize_input("bash", &serde_json::json!({"command": "ls -la"})),
            "ls -la"
        );
        assert_eq!(
            summarize_input(
                "write",
                &serde_json::json!({"path": "src/main.rs", "content": "x"})
            ),
            "src/main.rs"
        );
        // 알 수 없는 도구는 compact JSON 으로 폴백.
        let s = summarize_input("unknown", &serde_json::json!({"a": 1}));
        assert!(s.contains("\"a\""));
    }

    #[test]
    fn truncate_oneline_collapses_and_clips() {
        assert_eq!(truncate_oneline("a\nb\rc", 10), "a b c");
        let long = "x".repeat(200);
        let out = truncate_oneline(&long, 10);
        assert_eq!(out.chars().count(), 10);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn open_modal_shows_command_summary() {
        let mut app = App::new(SpinnerStyle::Ascii);
        let (tx, _rx) = tokio::sync::oneshot::channel();
        app.open_modal(PermissionRequest {
            tool: "bash".into(),
            input: serde_json::json!({"command": "rm -rf build"}),
            reply: tx,
        });
        assert_eq!(app.modal.as_ref().unwrap().summary, "rm -rf build");
    }

    #[test]
    fn parse_command_recognizes_slash_commands() {
        assert_eq!(
            parse_command("/provider openai"),
            Some(Command::Provider("openai".into()))
        );
        assert_eq!(
            parse_command("/p ollama"),
            Some(Command::Provider("ollama".into()))
        );
        assert_eq!(
            parse_command("/model gpt-4o"),
            Some(Command::Model("gpt-4o".into()))
        );
        assert_eq!(
            parse_command("/m qwen3.5:9b"),
            Some(Command::Model("qwen3.5:9b".into()))
        );
        assert_eq!(parse_command("/models"), Some(Command::Models));
        assert_eq!(parse_command("/providers"), Some(Command::Providers));
        assert_eq!(parse_command("/skills"), Some(Command::Skills));
        assert_eq!(parse_command("/help"), Some(Command::Help));
        // 스킬 호출(`/compact` 등)은 파서 단계에선 Unknown → 루프가 스킬 레지스트리로 확인.
        assert_eq!(
            parse_command("/compact"),
            Some(Command::Unknown("compact".into()))
        );
        // 인자 빠진 provider/model → Unknown(=help 유도).
        assert_eq!(
            parse_command("/provider"),
            Some(Command::Unknown("provider".into()))
        );
        assert_eq!(
            parse_command("/bogus"),
            Some(Command::Unknown("bogus".into()))
        );
        // 슬래시로 시작하지 않으면 일반 프롬프트.
        assert_eq!(parse_command("안녕"), None);
        assert_eq!(parse_command("  hi /not a command"), None);
    }

    // ── handle_command 용 최소 스텁(Agent 조립) ──
    use scv_core::context::NoopContextManager;
    use scv_core::message::Message;
    use scv_core::provider::{CompletionRequest, EventStream, ModelInfo, ToolSchema};
    use scv_core::tool::{PermissionGate, ToolContext, ToolRegistry};

    struct StubProvider(&'static str);
    #[async_trait::async_trait]
    impl Provider for StubProvider {
        fn id(&self) -> &str {
            self.0
        }
        fn models(&self) -> &[ModelInfo] {
            &[]
        }
        async fn stream(&self, _r: CompletionRequest) -> scv_core::Result<EventStream> {
            unreachable!()
        }
        async fn count_tokens(
            &self,
            _s: Option<&str>,
            _m: &[Message],
            _t: &[ToolSchema],
        ) -> scv_core::Result<u64> {
            Ok(0)
        }
    }
    fn model_info(id: &str) -> ModelInfo {
        ModelInfo {
            id: id.into(),
            context_window: 1_000_000,
            max_output_tokens: 64_000,
            supports_thinking: true,
        }
    }
    struct AllowGate;
    #[async_trait::async_trait]
    impl PermissionGate for AllowGate {
        async fn decide(&self, _t: &str, _i: &serde_json::Value) -> PermissionLevel {
            PermissionLevel::Allow
        }
    }
    fn stub_agent() -> Agent {
        Agent {
            provider: Arc::new(StubProvider("ollama")),
            tools: ToolRegistry::new(),
            permissions: Arc::new(AllowGate),
            context: Arc::new(NoopContextManager),
            model: "qwen3.5:9b".into(),
            system_prompt: String::new(),
            max_tokens: 16,
            effort: None,
            max_tool_iterations: 5,
            tool_ctx: ToolContext {
                workdir: std::env::temp_dir(),
                cancel: CancellationToken::new(),
            },
        }
    }

    #[test]
    fn handle_command_switches_provider_and_model() {
        let mut app = App::new(SpinnerStyle::Ascii);
        let mut agent = stub_agent();
        // 가짜 팩토리: openai → (openai provider, "gpt-5.5").
        let make = |id: &str| -> scv_core::Result<(Arc<dyn Provider>, String)> {
            match id {
                "openai" => Ok((Arc::new(StubProvider("openai")), "gpt-5.5".to_string())),
                other => Err(scv_core::Error::Provider(format!("no {other}"))),
            }
        };
        let skills = SkillRegistry::new();
        // /provider openai → provider·model 교체.
        app.handle_command(
            Command::Provider("openai".into()),
            &mut agent,
            &[],
            &make,
            &skills,
        );
        assert_eq!(agent.provider.id(), "openai");
        assert_eq!(agent.model, "gpt-5.5");
        assert_eq!(app.model_label, "openai·gpt-5.5");

        // /model 교체(프로바이더 유지).
        app.handle_command(
            Command::Model("gpt-5.4-mini".into()),
            &mut agent,
            &[],
            &make,
            &skills,
        );
        assert_eq!(agent.provider.id(), "openai");
        assert_eq!(agent.model, "gpt-5.4-mini");

        // 알 수 없는 프로바이더 → 실패 메시지, 상태 불변.
        app.handle_command(
            Command::Provider("nope".into()),
            &mut agent,
            &[],
            &make,
            &skills,
        );
        assert_eq!(agent.provider.id(), "openai"); // 그대로
        assert!(app.transcript.iter().any(|l| l.contains("switch failed")));
    }

    fn noop_make(_: &str) -> scv_core::Result<(Arc<dyn Provider>, String)> {
        Err(scv_core::Error::Provider("unused".into()))
    }

    #[test]
    fn model_command_validates_against_cached_catalog() {
        let mut app = App::new(SpinnerStyle::Ascii);
        let mut agent = stub_agent();
        agent.model = "claude-sonnet-4-6".into();
        // App 이 list_models 로 채우는 실시간 카탈로그를 흉내낸다.
        app.models = vec![
            model_info("claude-sonnet-4-6"),
            model_info("claude-opus-4-8"),
        ];
        let skills = SkillRegistry::new();

        // 카탈로그에 있는 모델 → 전환 성공.
        app.handle_command(
            Command::Model("claude-opus-4-8".into()),
            &mut agent,
            &[],
            &noop_make,
            &skills,
        );
        assert_eq!(agent.model, "claude-opus-4-8");
        assert!(app.transcript.last().unwrap().contains("switched model"));

        // 목록에 없는 모델(오타) → 거부, 모델 불변.
        app.handle_command(
            Command::Model("opus-4.8".into()),
            &mut agent,
            &[],
            &noop_make,
            &skills,
        );
        assert_eq!(agent.model, "claude-opus-4-8"); // 그대로
        assert!(app.transcript.last().unwrap().contains("unknown model"));
    }

    #[test]
    fn model_command_allows_any_when_catalog_empty() {
        // 카탈로그 조회 실패(빈 캐시) → 검증 불가 → 무엇이든 허용(하위호환).
        let mut app = App::new(SpinnerStyle::Ascii);
        let mut agent = stub_agent(); // app.models 는 기본 빈 벡터.
        let skills = SkillRegistry::new();
        app.handle_command(
            Command::Model("anything:latest".into()),
            &mut agent,
            &[],
            &noop_make,
            &skills,
        );
        assert_eq!(agent.model, "anything:latest");
    }

    #[test]
    fn models_command_lists_and_marks_current() {
        let mut app = App::new(SpinnerStyle::Ascii);
        let mut agent = stub_agent();
        agent.model = "claude-sonnet-4-6".into();
        app.models = vec![
            model_info("claude-sonnet-4-6"),
            model_info("claude-opus-4-8"),
        ];
        let skills = SkillRegistry::new();

        app.handle_command(Command::Models, &mut agent, &[], &noop_make, &skills);
        let last = app.transcript.last().unwrap();
        assert!(last.contains("claude-sonnet-4-6 (현재)"));
        assert!(last.contains("claude-opus-4-8"));

        // 캐시가 비면 안내 메시지.
        app.models.clear();
        app.handle_command(Command::Models, &mut agent, &[], &noop_make, &skills);
        assert!(app
            .transcript
            .last()
            .unwrap()
            .contains("등록된 모델이 없음"));
    }
}
