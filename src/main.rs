#![cfg_attr(all(not(debug_assertions), windows), windows_subsystem = "windows")]

use eframe::egui::{
    self, pos2, vec2, Align2, Color32, FontFamily, FontId, Painter, Pos2, Rect, RichText, Shape,
    Stroke,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::f32::consts::TAU;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// ───────────────────────── Configuration ─────────────────────────

const DEFAULT_PERSONA: &str = "Tu es Teto, une VTubeuse espiègle, chaleureuse et un peu chaotique, \
avec des couettes en forme de drill. Tu adores le pain (surtout les baguettes) et tu es fière de ton côté « chimère ». \
Tu parles à l'oral : réponses courtes (1 à 3 phrases), naturelles, sans markdown, sans listes, sans emojis. \
Tu tutoies l'utilisateur et tu peux le taquiner gentiment. Si on te pose une vraie question, tu réponds correctement.";

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
struct Config {
    api_key: String,
    model: String,
    persona: String,
    /// "fr" ou "ja"
    reply_lang: String,
    /// "system" ou "command"
    tts_mode: String,
    tts_command: String,
    rate: i32,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            api_key: String::new(),
            model: "gpt-4o-mini".to_string(),
            persona: DEFAULT_PERSONA.to_string(),
            reply_lang: "fr".to_string(),
            tts_mode: "system".to_string(),
            tts_command: String::new(),
            rate: 1,
        }
    }
}

fn config_path() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("teto").join("config.json")
}

fn load_config() -> Config {
    std::fs::read_to_string(config_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

// ───────────────────────── Événements / messages ─────────────────────────

enum Ev {
    Reply(String),
    Error(String),
    Heard(String),
    MicDone,
}

#[derive(Clone, Copy, PartialEq)]
enum Role {
    Me,
    Teto,
    Sys,
}

struct Msg {
    role: Role,
    text: String,
}

// ───────────────────────── OpenAI ─────────────────────────

fn system_prompt(cfg: &Config) -> String {
    let mut s = cfg.persona.clone();
    if cfg.reply_lang == "ja" {
        s.push_str(
            "\n\nIMPORTANT : réponds en japonais naturel (1 à 2 phrases courtes). \
Puis, sur une nouvelle ligne, écris « FR: » suivi de la traduction française.",
        );
    }
    s
}

fn call_openai(cfg: &Config, hist: &[(String, String)]) -> Result<String, String> {
    let mut msgs = vec![json!({"role": "system", "content": system_prompt(cfg)})];
    for (r, c) in hist {
        msgs.push(json!({"role": r, "content": c}));
    }
    let body = json!({
        "model": cfg.model,
        "messages": msgs,
        "temperature": 0.9
    });
    let resp = ureq::post("https://api.openai.com/v1/chat/completions")
        .set("Authorization", &format!("Bearer {}", cfg.api_key.trim()))
        .timeout(Duration::from_secs(60))
        .send_json(body);
    match resp {
        Ok(r) => {
            let v: Value = r.into_json().map_err(|e| e.to_string())?;
            v["choices"][0]["message"]["content"]
                .as_str()
                .map(|s| s.trim().to_string())
                .ok_or_else(|| "Réponse vide".to_string())
        }
        Err(ureq::Error::Status(code, r)) => {
            let v: Value = r.into_json().unwrap_or(Value::Null);
            let msg = v["error"]["message"].as_str().unwrap_or("").to_string();
            Err(format!("Erreur {} : {}", code, msg))
        }
        Err(e) => Err(e.to_string()),
    }
}

/// Sépare l'affichage (tout) de ce qui est dit à voix haute.
fn speech_part(text: &str, ja: bool) -> String {
    let mut spoken = String::new();
    for line in text.lines() {
        let l = line.trim();
        if ja && (l.starts_with("FR:") || l.starts_with("FR :")) {
            break;
        }
        spoken.push_str(l);
        spoken.push(' ');
    }
    sanitize(&spoken)
}

/// Retire ce qui gêne la voix ou une ligne de commande.
fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '"' | '&' | '|' | '<' | '>' | '^' | '%' | '`' | '*' | '#' | '_' | '\n' | '\r' => ' ',
            c => c,
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

// ───────────────────────── Voix ─────────────────────────

#[cfg(windows)]
fn hide_window(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
}
#[cfg(not(windows))]
fn hide_window(_cmd: &mut Command) {}

/// Lance un processus, le range dans `slot` (pour pouvoir le couper) et l'attend.
fn run_and_wait(
    slot: &Arc<Mutex<Option<Child>>>,
    gen: &Arc<AtomicUsize>,
    my: usize,
    mut cmd: Command,
) -> bool {
    hide_window(&mut cmd);
    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(_) => return false,
    };
    *slot.lock().unwrap() = Some(child);
    loop {
        std::thread::sleep(Duration::from_millis(50));
        if gen.load(Ordering::SeqCst) != my {
            return false;
        }
        let mut g = slot.lock().unwrap();
        match g.as_mut() {
            None => return false,
            Some(c) => match c.try_wait() {
                Ok(Some(st)) => {
                    *g = None;
                    return st.success();
                }
                Ok(None) => {}
                Err(_) => {
                    *g = None;
                    return false;
                }
            },
        }
    }
}

fn run_tts(
    cfg: &Config,
    text: &str,
    slot: &Arc<Mutex<Option<Child>>>,
    gen: &Arc<AtomicUsize>,
    my: usize,
    tx: &Sender<Ev>,
) {
    if text.is_empty() {
        return;
    }
    if cfg.tts_mode == "command" && !cfg.tts_command.trim().is_empty() {
        // Voix externe (ex. logiciel de synthèse avec la voix de Teto)
        let out = std::env::temp_dir().join("teto_voice.wav");
        let _ = std::fs::remove_file(&out);
        let line = cfg
            .tts_command
            .replace("{text}", text)
            .replace("{out}", &out.to_string_lossy());
        let mut c = Command::new("cmd");
        c.args(["/C", &line]);
        run_and_wait(slot, gen, my, c);
        if gen.load(Ordering::SeqCst) != my {
            return;
        }
        if !out.exists() {
            let _ = tx.send(Ev::Error(
                "La commande de voix n'a produit aucun fichier WAV ({out}). Vérifie-la dans les Réglages."
                    .to_string(),
            ));
            return;
        }
        let mut p = Command::new("powershell");
        p.args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "(New-Object Media.SoundPlayer $env:TETO_WAV).PlaySync()",
        ])
        .env("TETO_WAV", &out);
        run_and_wait(slot, gen, my, p);
    } else {
        // Voix Windows intégrée (SAPI)
        let culture = if cfg.reply_lang == "ja" { "ja-JP" } else { "fr-FR" };
        let script = format!(
            "Add-Type -AssemblyName System.Speech; \
             $s = New-Object System.Speech.Synthesis.SpeechSynthesizer; \
             try {{ $s.SelectVoiceByHints('Female','NotSet',0,[cultureinfo]'{}') }} catch {{}}; \
             $s.Rate = {}; $s.Speak($env:TETO_TEXT)",
            culture, cfg.rate
        );
        let mut p = Command::new("powershell");
        p.args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .env("TETO_TEXT", text);
        run_and_wait(slot, gen, my, p);
    }
}

const MIC_SCRIPT: &str = "Add-Type -AssemblyName System.Speech; \
$c = [System.Globalization.CultureInfo]'fr-FR'; \
$r = New-Object System.Speech.Recognition.SpeechRecognitionEngine($c); \
$r.SetInputToDefaultAudioDevice(); \
$r.LoadGrammar((New-Object System.Speech.Recognition.DictationGrammar)); \
$res = $r.Recognize([TimeSpan]::FromSeconds(8)); \
if ($res) { [Console]::OutputEncoding = [Text.Encoding]::UTF8; Write-Output $res.Text }";

// ───────────────────────── Application ─────────────────────────

struct TetoApp {
    cfg: Config,
    input: String,
    log: Vec<Msg>,
    history: Vec<(String, String)>,
    show_settings: bool,
    save_note: Option<String>,
    busy: bool,
    listening: bool,
    speaking: Arc<AtomicBool>,
    slot: Arc<Mutex<Option<Child>>>,
    gen: Arc<AtomicUsize>,
    tx: Sender<Ev>,
    rx: Receiver<Ev>,
    start: Instant,
    next_blink: f32,
}

fn setup_fonts(ctx: &egui::Context) {
    // Police système pour pouvoir afficher le japonais.
    let mut fonts = egui::FontDefinitions::default();
    for p in [
        "C:\\Windows\\Fonts\\YuGothM.ttc",
        "C:\\Windows\\Fonts\\meiryo.ttc",
        "C:\\Windows\\Fonts\\msgothic.ttc",
    ] {
        if let Ok(bytes) = std::fs::read(p) {
            fonts
                .font_data
                .insert("jp".to_owned(), egui::FontData::from_owned(bytes));
            for fam in [FontFamily::Proportional, FontFamily::Monospace] {
                if let Some(list) = fonts.families.get_mut(&fam) {
                    list.push("jp".to_owned());
                }
            }
            break;
        }
    }
    ctx.set_fonts(fonts);
}

impl TetoApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        setup_fonts(&cc.egui_ctx);
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        let cfg = load_config();
        let (tx, rx) = channel();
        let mut log = Vec::new();
        let first = if cfg.api_key.trim().is_empty() {
            "Bienvenue ! Ouvre les Réglages pour ajouter ta clé API OpenAI."
        } else {
            "Salut ! Écris-moi ou clique sur Micro."
        };
        log.push(Msg { role: Role::Sys, text: first.to_string() });
        TetoApp {
            show_settings: cfg.api_key.trim().is_empty(),
            cfg,
            input: String::new(),
            log,
            history: Vec::new(),
            save_note: None,
            busy: false,
            listening: false,
            speaking: Arc::new(AtomicBool::new(false)),
            slot: Arc::new(Mutex::new(None)),
            gen: Arc::new(AtomicUsize::new(0)),
            tx,
            rx,
            start: Instant::now(),
            next_blink: 2.0,
        }
    }

    fn save(&mut self) {
        let path = config_path();
        let res: Result<(), String> = (|| {
            if let Some(d) = path.parent() {
                std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
            }
            let s = serde_json::to_string_pretty(&self.cfg).map_err(|e| e.to_string())?;
            std::fs::write(&path, s).map_err(|e| e.to_string())
        })();
        self.save_note = Some(match res {
            Ok(()) => format!("Enregistré dans {}", path.display()),
            Err(e) => format!("Échec de l'enregistrement : {}", e),
        });
    }

    fn stop_speaking(&self) {
        self.gen.fetch_add(1, Ordering::SeqCst);
        if let Some(mut c) = self.slot.lock().unwrap().take() {
            let _ = c.kill();
        }
        self.speaking.store(false, Ordering::SeqCst);
    }

    fn speak(&self, ctx: &egui::Context, text: String) {
        self.stop_speaking();
        let my = self.gen.load(Ordering::SeqCst);
        let cfg = self.cfg.clone();
        let flag = self.speaking.clone();
        let slot = self.slot.clone();
        let gen = self.gen.clone();
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            flag.store(true, Ordering::SeqCst);
            run_tts(&cfg, &text, &slot, &gen, my, &tx);
            if gen.load(Ordering::SeqCst) == my {
                flag.store(false, Ordering::SeqCst);
            }
            ctx.request_repaint();
        });
    }

    fn send(&mut self, ctx: &egui::Context, text: String) {
        if self.busy {
            return;
        }
        if self.cfg.api_key.trim().is_empty() {
            self.log.push(Msg {
                role: Role::Sys,
                text: "Ajoute ta clé API OpenAI dans les Réglages.".to_string(),
            });
            self.show_settings = true;
            return;
        }
        self.log.push(Msg { role: Role::Me, text: text.clone() });
        self.history.push(("user".to_string(), text));
        if self.history.len() > 20 {
            let n = self.history.len() - 20;
            self.history.drain(0..n);
        }
        self.busy = true;
        let cfg = self.cfg.clone();
        let hist = self.history.clone();
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let ev = match call_openai(&cfg, &hist) {
                Ok(t) => Ev::Reply(t),
                Err(e) => Ev::Error(e),
            };
            let _ = tx.send(ev);
            ctx.request_repaint();
        });
    }

    fn start_mic(&mut self, ctx: &egui::Context) {
        if self.listening {
            return;
        }
        self.stop_speaking();
        self.listening = true;
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let mut c = Command::new("powershell");
            c.args(["-NoProfile", "-NonInteractive", "-Command", MIC_SCRIPT]);
            hide_window(&mut c);
            match c.output() {
                Ok(o) => {
                    let t = String::from_utf8_lossy(&o.stdout).trim().to_string();
                    if t.is_empty() {
                        let _ = tx.send(Ev::MicDone);
                    } else {
                        let _ = tx.send(Ev::Heard(t));
                    }
                }
                Err(e) => {
                    let _ = tx.send(Ev::Error(format!("Micro : {}", e)));
                }
            }
            ctx.request_repaint();
        });
    }

    fn handle(&mut self, ctx: &egui::Context, ev: Ev) {
        match ev {
            Ev::Reply(t) => {
                self.busy = false;
                self.history.push(("assistant".to_string(), t.clone()));
                let spoken = speech_part(&t, self.cfg.reply_lang == "ja");
                self.log.push(Msg { role: Role::Teto, text: t });
                self.speak(ctx, spoken);
            }
            Ev::Error(e) => {
                self.busy = false;
                self.listening = false;
                self.log.push(Msg { role: Role::Sys, text: e });
            }
            Ev::Heard(t) => {
                self.listening = false;
                self.send(ctx, t);
            }
            Ev::MicDone => {
                self.listening = false;
                self.log.push(Msg {
                    role: Role::Sys,
                    text: "Je n'ai rien entendu (micro expérimental, il faut la reconnaissance vocale Windows en français)."
                        .to_string(),
                });
            }
        }
    }
}

// ───────────────────────── Dessin de Teto ─────────────────────────

struct View {
    o: Pos2,
    s: f32,
}
impl View {
    fn p(&self, x: f32, y: f32) -> Pos2 {
        pos2(self.o.x + x * self.s, self.o.y + y * self.s)
    }
}

fn hex(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}

fn poly(p: &Painter, pts: Vec<Pos2>, col: Color32) {
    p.add(Shape::convex_polygon(pts, col, Stroke::NONE));
}

fn line(p: &Painter, pts: Vec<Pos2>, w: f32, col: Color32) {
    p.add(Shape::line(pts, Stroke::new(w, col)));
}

fn ell(p: &Painter, v: &View, cx: f32, cy: f32, rx: f32, ry: f32, col: Color32) {
    let n = 40;
    let pts: Vec<Pos2> = (0..n)
        .map(|i| {
            let a = i as f32 / n as f32 * TAU;
            v.p(cx + rx * a.cos(), cy + ry * a.sin())
        })
        .collect();
    poly(p, pts, col);
}

fn quad(v: &View, a: (f32, f32), c: (f32, f32), b: (f32, f32)) -> Vec<Pos2> {
    (0..=14)
        .map(|i| {
            let t = i as f32 / 14.0;
            let u = 1.0 - t;
            v.p(
                u * u * a.0 + 2.0 * u * t * c.0 + t * t * b.0,
                u * u * a.1 + 2.0 * u * t * c.1 + t * t * b.1,
            )
        })
        .collect()
}

fn rot(x: f32, y: f32, px: f32, py: f32, a: f32) -> (f32, f32) {
    let (s, c) = a.sin_cos();
    let (dx, dy) = (x - px, y - py);
    (px + dx * c - dy * s, py + dx * s + dy * c)
}

fn interp(side: &[(f32, f32)], y: f32) -> f32 {
    for w in side.windows(2) {
        let (a, b) = (w[0], w[1]);
        if y >= a.1 && y <= b.1 {
            let t = (y - a.1) / (b.1 - a.1);
            return a.0 + (b.0 - a.0) * t;
        }
    }
    side.last().map(|p| p.0).unwrap_or(0.0)
}

fn drill(p: &Painter, v: &View, mirror: bool, ang: f32, bob: f32) {
    let l = [(70.0, 150.0), (72.0, 250.0), (84.0, 350.0), (100.0, 480.0)];
    let r = [(150.0, 150.0), (145.0, 250.0), (128.0, 350.0), (100.0, 480.0)];
    let tf = |(x, y): (f32, f32)| -> Pos2 {
        let (x, y) = rot(x, y, 110.0, 150.0, ang);
        let x = if mirror { 400.0 - x } else { x };
        v.p(x, y + bob)
    };
    let pts = vec![
        tf(l[0]),
        tf(r[0]),
        tf(r[1]),
        tf(r[2]),
        tf(r[3]),
        tf(l[2]),
        tf(l[1]),
    ];
    poly(p, pts, hex(217, 58, 88));
    for y in [210.0_f32, 270.0, 330.0, 390.0, 440.0] {
        let xl = interp(&l, y);
        let xr = interp(&r, y);
        line(
            p,
            vec![tf((xl, y)), tf(((xl + xr) / 2.0, y + 12.0)), tf((xr, y))],
            5.0 * v.s,
            hex(161, 29, 58),
        );
    }
}

fn draw_teto(
    p: &Painter,
    rect: Rect,
    t: f32,
    mouse: Option<Pos2>,
    speaking: bool,
    blink: f32,
) {
    let s = (rect.width() / 400.0).min(rect.height() / 560.0) * 0.96;
    let o = pos2(
        rect.center().x - 200.0 * s,
        rect.center().y - 280.0 * s,
    );
    let v = View { o, s };
    let bob = 5.0 * (t * 2.0).sin();
    let sway = 0.07 * (t * 1.5).sin();
    let skin = hex(253, 227, 211);
    let hair = hex(232, 64, 92);

    // couettes
    drill(p, &v, false, sway, bob);
    drill(p, &v, true, sway, bob);

    // cou
    p.rect_filled(
        Rect::from_min_max(v.p(182.0, 330.0 + bob), v.p(218.0, 390.0 + bob)),
        6.0 * s,
        hex(251, 217, 200),
    );
    // corps
    poly(
        p,
        vec![
            v.p(120.0, 570.0),
            v.p(125.0, 430.0 + bob),
            v.p(150.0, 392.0 + bob),
            v.p(200.0, 382.0 + bob),
            v.p(250.0, 392.0 + bob),
            v.p(275.0, 430.0 + bob),
            v.p(280.0, 570.0),
        ],
        hex(51, 40, 63),
    );
    poly(
        p,
        vec![v.p(165.0, 388.0 + bob), v.p(200.0, 432.0 + bob), v.p(235.0, 388.0 + bob)],
        hex(244, 240, 246),
    );
    poly(
        p,
        vec![
            v.p(186.0, 430.0 + bob),
            v.p(214.0, 430.0 + bob),
            v.p(200.0, 452.0 + bob),
        ],
        hex(224, 49, 79),
    );

    // tête
    ell(p, &v, 200.0, 215.0 + bob, 88.0, 100.0, skin);
    ell(p, &v, 146.0, 248.0 + bob, 14.0, 8.0, Color32::from_rgba_unmultiplied(255, 154, 168, 120));
    ell(p, &v, 254.0, 248.0 + bob, 14.0, 8.0, Color32::from_rgba_unmultiplied(255, 154, 168, 120));

    // yeux
    let (dx, dy) = match mouse {
        Some(m) => (
            ((m.x - rect.center().x) / (rect.width() / 2.0)).clamp(-1.0, 1.0),
            ((m.y - rect.center().y) / (rect.height() / 2.0)).clamp(-1.0, 1.0),
        ),
        None => (0.0, 0.0),
    };
    for cx in [160.0_f32, 240.0] {
        ell(p, &v, cx, 214.0 + bob, 20.0, 26.0, Color32::WHITE);
        ell(p, &v, cx + dx * 5.0, 218.0 + bob + dy * 4.0, 14.0, 20.0, hex(212, 32, 61));
        ell(p, &v, cx + dx * 5.0, 218.0 + bob + dy * 4.0, 7.0, 10.0, hex(59, 15, 27));
        ell(p, &v, cx - 6.0 + dx * 5.0, 209.0 + bob + dy * 4.0, 5.0, 5.0, Color32::WHITE);
        if blink > 0.0 {
            p.rect_filled(
                Rect::from_min_max(
                    v.p(cx - 24.0, 186.0 + bob),
                    v.p(cx + 24.0, 186.0 + bob + 56.0 * blink),
                ),
                0.0,
                skin,
            );
        }
        line(
            p,
            quad(&v, (cx - 22.0, 190.0 + bob), (cx, 176.0 + bob), (cx + 22.0, 190.0 + bob)),
            5.0 * s,
            hex(140, 23, 48),
        );
    }

    // bouche
    if speaking {
        let a = (t * 11.0).sin().abs() * (0.55 + 0.45 * (t * 3.1).sin().abs());
        ell(p, &v, 200.0, 280.0 + bob, 11.0 + a * 4.0, 2.0 + a * 14.0, hex(122, 30, 46));
    } else {
        line(
            p,
            quad(&v, (186.0, 280.0 + bob), (200.0, 289.0 + bob), (214.0, 280.0 + bob)),
            4.0 * s,
            hex(161, 57, 74),
        );
    }

    // cheveux (calotte + frange)
    let cap: Vec<Pos2> = (0..=24)
        .map(|i| {
            let a = std::f32::consts::PI + i as f32 / 24.0 * std::f32::consts::PI;
            v.p(200.0 + 92.0 * a.cos(), 175.0 + bob + 82.0 * a.sin())
        })
        .collect();
    poly(p, cap, hair);
    for tri in [
        [(110.0, 170.0), (150.0, 170.0), (122.0, 212.0)],
        [(140.0, 170.0), (200.0, 170.0), (170.0, 192.0)],
        [(190.0, 170.0), (250.0, 170.0), (222.0, 186.0)],
        [(240.0, 170.0), (290.0, 170.0), (280.0, 212.0)],
    ] {
        poly(
            p,
            tri.iter().map(|&(x, y)| v.p(x, y + bob)).collect(),
            hair,
        );
    }
}

// ───────────────────────── Interface ─────────────────────────

impl eframe::App for TetoApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        while let Ok(ev) = self.rx.try_recv() {
            self.handle(ctx, ev);
        }
        ctx.request_repaint_after(Duration::from_millis(16));

        let t = self.start.elapsed().as_secs_f32();
        let speaking = self.speaking.load(Ordering::SeqCst);

        // clignement
        let mut blink = 0.0;
        if t >= self.next_blink {
            let k = (t - self.next_blink) / 0.18;
            if k >= 1.0 {
                let r = ((t * 12.9898).sin() * 43758.5453).fract().abs();
                self.next_blink = t + 2.2 + r * 3.5;
            } else {
                blink = 1.0 - (2.0 * k - 1.0).abs();
            }
        }

        let mut to_send: Option<String> = None;
        let mut want_mic = false;
        let mut want_stop = false;

        // ── panneau de discussion
        egui::SidePanel::right("chat")
            .min_width(340.0)
            .default_width(420.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.heading("Teto");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Réglages").clicked() {
                            self.show_settings = true;
                        }
                        if ui.button("Couper la voix").clicked() {
                            want_stop = true;
                        }
                        let label = if self.listening { "J'écoute…" } else { "Micro" };
                        if ui
                            .add_enabled(!self.listening, egui::Button::new(label))
                            .clicked()
                        {
                            want_mic = true;
                        }
                    });
                });
                ui.separator();

                egui::TopBottomPanel::bottom("input_bar").show_inside(ui, |ui| {
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        let resp = ui.add(
                            egui::TextEdit::singleline(&mut self.input)
                                .hint_text("Écris à Teto…")
                                .desired_width(ui.available_width() - 90.0),
                        );
                        let enter = resp.lost_focus()
                            && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        let click = ui.button("Envoyer").clicked();
                        if (enter || click) && !self.input.trim().is_empty() {
                            to_send = Some(std::mem::take(&mut self.input));
                            resp.request_focus();
                        }
                    });
                    ui.add_space(6.0);
                });

                egui::CentralPanel::default().show_inside(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .stick_to_bottom(true)
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            for m in &self.log {
                                match m.role {
                                    Role::Me => {
                                        ui.label(
                                            RichText::new("Toi")
                                                .strong()
                                                .color(hex(120, 150, 255)),
                                        );
                                        ui.label(&m.text);
                                    }
                                    Role::Teto => {
                                        ui.label(
                                            RichText::new("Teto").strong().color(hex(255, 107, 134)),
                                        );
                                        ui.label(&m.text);
                                    }
                                    Role::Sys => {
                                        ui.label(RichText::new(&m.text).weak().italics());
                                    }
                                }
                                ui.add_space(8.0);
                            }
                            if self.busy {
                                ui.label(RichText::new("Teto réfléchit…").weak());
                            }
                        });
                });
            });

        // ── scène
        let mouse = ctx.input(|i| i.pointer.hover_pos());
        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(hex(27, 20, 36)))
            .show(ctx, |ui| {
                let rect = ui.max_rect();
                let p = ui.painter();
                p.circle_filled(
                    rect.center() + vec2(0.0, rect.height() * 0.1),
                    rect.height().min(rect.width()) * 0.42,
                    hex(48, 30, 66),
                );
                draw_teto(p, rect, t, mouse, speaking, blink);
                let status = if speaking {
                    "Teto parle…"
                } else if self.busy {
                    "Teto réfléchit…"
                } else if self.listening {
                    "Je t'écoute…"
                } else {
                    "Prête."
                };
                p.text(
                    rect.left_bottom() + vec2(14.0, -12.0),
                    Align2::LEFT_BOTTOM,
                    status,
                    FontId::proportional(14.0),
                    Color32::GRAY,
                );
            });

        // ── réglages
        let mut open = self.show_settings;
        let mut do_save = false;
        egui::Window::new("Réglages")
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_width(460.0)
            .show(ctx, |ui| {
                ui.label("Clé API OpenAI");
                ui.add(
                    egui::TextEdit::singleline(&mut self.cfg.api_key)
                        .password(true)
                        .hint_text("sk-…")
                        .desired_width(f32::INFINITY),
                );
                ui.add_space(6.0);
                ui.label("Modèle");
                ui.add(egui::TextEdit::singleline(&mut self.cfg.model).desired_width(f32::INFINITY));
                ui.add_space(6.0);

                ui.label("Langue des réponses");
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.cfg.reply_lang, "fr".to_string(), "Français");
                    ui.selectable_value(
                        &mut self.cfg.reply_lang,
                        "ja".to_string(),
                        "Japonais (+ traduction affichée)",
                    );
                });
                ui.add_space(6.0);

                ui.label("Voix");
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.cfg.tts_mode, "system".to_string(), "Voix Windows");
                    ui.selectable_value(
                        &mut self.cfg.tts_mode,
                        "command".to_string(),
                        "Commande externe (voix de Teto)",
                    );
                });
                if self.cfg.tts_mode == "command" {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.cfg.tts_command)
                            .hint_text(r#"voicepeak -s "{text}" -n "NOM DE LA VOIX" -o "{out}""#)
                            .desired_width(f32::INFINITY),
                    );
                    ui.label(
                        RichText::new(
                            "{text} = le texte à dire, {out} = le fichier WAV que ta commande doit créer.",
                        )
                        .weak()
                        .small(),
                    );
                } else {
                    ui.add(egui::Slider::new(&mut self.cfg.rate, -5..=5).text("Vitesse"));
                }
                ui.add_space(6.0);

                ui.label("Personnalité");
                ui.add(
                    egui::TextEdit::multiline(&mut self.cfg.persona)
                        .desired_rows(6)
                        .desired_width(f32::INFINITY),
                );
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Enregistrer").clicked() {
                        do_save = true;
                    }
                    if ui.button("Personnalité par défaut").clicked() {
                        self.cfg.persona = DEFAULT_PERSONA.to_string();
                    }
                });
                if let Some(n) = &self.save_note {
                    ui.label(RichText::new(n).color(hex(110, 200, 140)));
                }
            });
        if do_save {
            self.save();
        }
        if self.show_settings && !open {
            self.save(); // fermeture par la croix : on enregistre aussi
        }
        self.show_settings = open;

        if want_stop {
            self.stop_speaking();
        }
        if want_mic {
            self.start_mic(ctx);
        }
        if let Some(text) = to_send {
            self.send(ctx, text);
        }
    }
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Teto")
            .with_inner_size([980.0, 680.0])
            .with_min_inner_size([700.0, 480.0]),
        ..Default::default()
    };
    eframe::run_native("Teto", options, Box::new(|cc| Ok(Box::new(TetoApp::new(cc)))))
}
