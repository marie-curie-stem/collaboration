//! Terminal dino runner for an 80x25 window.
//!
//!   SPACE / Up   jump (also starts and restarts the game)
//!   e / Esc      exit

use std::io::{self, Write};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crossterm::cursor::{Hide, MoveTo, Show};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::style::Print;
use crossterm::terminal::{self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{execute, queue};

const W: usize = 80;
const H: usize = 25;
/// Row of the ground line; the dino's feet rest on the row above it.
const GROUND_ROW: usize = 21;
const DINO_X: i32 = 6;
const FRAME: Duration = Duration::from_millis(50); // 20 frames per second
const GRAVITY: f32 = 0.18;
const JUMP_VELOCITY: f32 = 1.4;
const START_SPEED: f32 = 0.9; // columns per frame
const MAX_SPEED: f32 = 2.0;
/// Frames after a crash before SPACE restarts (so mashing doesn't skip the screen).
const RESTART_LOCKOUT: u32 = 10;

// ---------------------------------------------------------------- randomness

struct Rng(u64);

impl Rng {
    fn new() -> Self {
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x9E37_79B9_7F4A_7C15);
        let mut r = Rng(seed | 1);
        for _ in 0..8 {
            r.next();
        }
        r
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn range(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn chance(&mut self, num: u64, den: u64) -> bool {
        self.range(den) < num
    }
}

/// Stable per-column hash, used for the scrolling ground texture.
fn hash(n: i64) -> u64 {
    let mut x = n as u64;
    x = x.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    x ^= x >> 29;
    x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x ^ (x >> 32)
}

// ---------------------------------------------------------------- game state

#[derive(PartialEq, Clone, Copy)]
enum State {
    Ready,
    Playing,
    Over,
}

struct Obstacle {
    x: f32,
    units: usize, // 1 or 2 cacti side by side
    h: usize,     // 2 or 3 rows tall
}

impl Obstacle {
    fn width(&self) -> usize {
        self.units * 3
    }
}

struct Cloud {
    x: f32,
    y: usize,
}

struct Game {
    rng: Rng,
    state: State,
    dino_y: f32, // height of the dino's feet above the ground, in rows
    vel: f32,
    airborne: bool,
    obstacles: Vec<Obstacle>,
    clouds: Vec<Cloud>,
    speed: f32,
    score: f32,
    hi: u32,
    frame: u64,
    scroll: f32,
    since_spawn: f32,
    next_gap: f32,
    over_frames: u32,
}

impl Game {
    fn new() -> Self {
        let mut g = Game {
            rng: Rng::new(),
            state: State::Ready,
            dino_y: 0.0,
            vel: 0.0,
            airborne: false,
            obstacles: Vec::new(),
            clouds: Vec::new(),
            speed: START_SPEED,
            score: 0.0,
            hi: 0,
            frame: 0,
            scroll: 0.0,
            since_spawn: 0.0,
            next_gap: 0.0,
            over_frames: 0,
        };
        g.reset();
        g
    }

    /// Back to the title screen; the high score is kept.
    fn reset(&mut self) {
        self.state = State::Ready;
        self.dino_y = 0.0;
        self.vel = 0.0;
        self.airborne = false;
        self.obstacles.clear();
        self.clouds.clear();
        self.speed = START_SPEED;
        self.score = 0.0;
        self.frame = 0;
        self.scroll = 0.0;
        self.since_spawn = 1000.0; // spawn the first cactus right away
        self.next_gap = 0.0;
        self.over_frames = 0;
        for i in 0..3 {
            let x = 10.0 + i as f32 * 25.0 + self.rng.range(10) as f32;
            let y = 3 + self.rng.range(6) as usize;
            self.clouds.push(Cloud { x, y });
        }
    }

    fn jump(&mut self) {
        if !self.airborne {
            self.airborne = true;
            self.vel = JUMP_VELOCITY;
        }
    }

    fn press_jump(&mut self) {
        match self.state {
            State::Ready => {
                self.state = State::Playing;
                self.jump();
            }
            State::Playing => self.jump(),
            State::Over => {
                if self.over_frames >= RESTART_LOCKOUT {
                    self.reset();
                    self.state = State::Playing;
                    self.jump();
                }
            }
        }
    }

    fn spawn(&mut self) {
        let h = if self.rng.chance(1, 2) { 2 } else { 3 };
        // Double cacti only once the game is a little faster.
        let units = if self.speed >= 1.1 && self.rng.chance(1, 3) { 2 } else { 1 };
        let o = Obstacle { x: W as f32, units, h };
        let w = o.width() as f32;
        self.obstacles.push(o);
        self.since_spawn = 0.0;
        // Distance (left edge to left edge) to the next cactus: always leaves
        // room to land and jump again, scaled with the current speed.
        self.next_gap = w + 16.0 * self.speed + 10.0 + self.rng.range(30) as f32;
    }

    fn collides(&self) -> bool {
        let left = DINO_X as f32 + 1.0;
        let right = DINO_X as f32 + 5.0;
        self.obstacles.iter().any(|o| {
            let ol = o.x + 0.5;
            let or = o.x + o.width() as f32 - 0.5;
            ol < right && or > left && self.dino_y < o.h as f32 - 0.5
        })
    }

    fn update(&mut self) {
        self.frame += 1;
        match self.state {
            State::Ready => return,
            State::Over => {
                self.over_frames += 1;
                return;
            }
            State::Playing => {}
        }

        // Dino physics.
        if self.airborne {
            self.dino_y += self.vel;
            self.vel -= GRAVITY;
            if self.dino_y <= 0.0 {
                self.dino_y = 0.0;
                self.vel = 0.0;
                self.airborne = false;
            }
        }

        // World movement.
        let s = self.speed;
        self.scroll += s;
        for o in &mut self.obstacles {
            o.x -= s;
        }
        self.obstacles.retain(|o| o.x + o.width() as f32 > -1.0);
        for c in &mut self.clouds {
            c.x -= s * 0.3;
        }
        self.clouds.retain(|c| c.x > -12.0);
        if self.clouds.len() < 4 && self.rng.chance(1, 40) {
            let y = 3 + self.rng.range(6) as usize;
            self.clouds.push(Cloud { x: W as f32, y });
        }

        self.since_spawn += s;
        if self.since_spawn >= self.next_gap {
            self.spawn();
        }

        if self.collides() {
            self.state = State::Over;
            self.over_frames = 0;
            self.hi = self.hi.max(self.score as u32);
            return;
        }

        self.score += s * 0.4;
        self.speed = (self.speed + 0.0005).min(MAX_SPEED);
    }

    // ------------------------------------------------------------- rendering

    fn render(&self) -> Vec<Vec<char>> {
        let mut b = vec![vec![' '; W]; H];

        // HUD
        put(&mut b, 2, 0, "DINO RUNNER");
        let hud = format!("HI {:05}   {:05}", self.hi, self.score as u32);
        put(&mut b, (W - 2 - hud.len()) as i32, 0, &hud);
        put(&mut b, 2, (H - 1) as i32, "SPACE: jump    E / Esc: exit");

        // Clouds
        for c in &self.clouds {
            put(&mut b, c.x as i32, c.y as i32, "_.-~~-._");
        }

        // Ground line and scrolling texture
        for x in 0..W {
            b[GROUND_ROW][x] = '=';
            let wx = x as i64 + self.scroll as i64;
            b[GROUND_ROW + 1][x] = match hash(wx) % 19 {
                0 => '.',
                1 => ',',
                2 => '\'',
                _ => ' ',
            };
            b[GROUND_ROW + 2][x] = match hash(wx + 7919) % 23 {
                0 => '.',
                1 => '`',
                2 => '-',
                _ => ' ',
            };
        }

        // Cacti
        for o in &self.obstacles {
            let rows: &[&str] = if o.h == 2 {
                &["\\|/", " | "]
            } else {
                &[" | ", "\\|/", " | "]
            };
            for u in 0..o.units {
                let x0 = o.x.round() as i32 + 3 * u as i32;
                for (i, line) in rows.iter().enumerate() {
                    let y = GROUND_ROW as i32 - o.h as i32 + i as i32;
                    put(&mut b, x0, y, line);
                }
            }
        }

        // Dino
        let yb = GROUND_ROW as i32 - 1 - self.dino_y.round() as i32;
        let eye = if self.state == State::Over { 'x' } else { 'o' };
        let legs = if self.airborne {
            " ||"
        } else if self.state == State::Playing && (self.frame / 2) % 2 == 1 {
            " \\/"
        } else {
            " /\\"
        };
        put(&mut b, DINO_X, yb - 2, &format!("  ({}>", eye));
        put(&mut b, DINO_X, yb - 1, "_/||");
        put(&mut b, DINO_X, yb, legs);

        // Overlays
        match self.state {
            State::Ready => put_center(&mut b, 9, "Press SPACE to start"),
            State::Over => {
                put_center(&mut b, 9, "G A M E   O V E R");
                if self.over_frames >= RESTART_LOCKOUT {
                    put_center(&mut b, 11, "SPACE: play again    E / Esc: exit");
                }
            }
            State::Playing => {}
        }

        b
    }
}

/// Draw `s` at (x, y), clipping to the screen. Spaces are transparent.
fn put(buf: &mut [Vec<char>], x: i32, y: i32, s: &str) {
    if y < 0 || y >= H as i32 {
        return;
    }
    for (i, ch) in s.chars().enumerate() {
        let cx = x + i as i32;
        if ch != ' ' && cx >= 0 && cx < W as i32 {
            buf[y as usize][cx as usize] = ch;
        }
    }
}

fn put_center(buf: &mut [Vec<char>], y: i32, s: &str) {
    let x = (W as i32 - s.chars().count() as i32) / 2;
    put(buf, x, y, s);
}

// ---------------------------------------------------------------- terminal

/// Puts the terminal into raw/alternate-screen mode and restores it on drop,
/// including on panic or early return.
struct TermGuard;

impl TermGuard {
    fn new() -> io::Result<Self> {
        terminal::enable_raw_mode()?;
        execute!(io::stdout(), EnterAlternateScreen, Hide, Clear(ClearType::All))?;
        Ok(TermGuard)
    }
}

impl Drop for TermGuard {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), Show, LeaveAlternateScreen);
        let _ = terminal::disable_raw_mode();
    }
}

fn draw(out: &mut impl Write, game: &Game, last_size: &mut (u16, u16)) -> io::Result<()> {
    let (cols, rows) = terminal::size()?;
    if (cols, rows) != *last_size {
        queue!(out, Clear(ClearType::All))?;
        *last_size = (cols, rows);
    }
    if (cols as usize) < W || (rows as usize) < H {
        let msg = format!("Terminal too small: need {W}x{H}, have {cols}x{rows}");
        queue!(out, MoveTo(0, 0), Print(msg), Clear(ClearType::UntilNewLine))?;
        return out.flush();
    }

    // Centre the 80x25 playfield if the terminal is larger.
    let ox = (cols as usize - W) / 2;
    let oy = (rows as usize - H) / 2;
    let buf = game.render();
    for (y, row) in buf.iter().enumerate() {
        // Skip the very last cell of the last row so an exactly 80x25
        // terminal never scrolls.
        let line: String = if y == H - 1 {
            row[..W - 1].iter().collect()
        } else {
            row.iter().collect()
        };
        queue!(out, MoveTo(ox as u16, (oy + y) as u16), Print(line))?;
    }
    out.flush()
}

fn main() -> io::Result<()> {
    // Restore the terminal before a panic message is printed.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = execute!(io::stdout(), Show, LeaveAlternateScreen);
        let _ = terminal::disable_raw_mode();
        default_hook(info);
    }));

    let _guard = TermGuard::new()?;
    let mut out = io::stdout();
    let mut game = Game::new();
    let mut last_size = (0u16, 0u16);

    loop {
        // Collect input until the next frame is due.
        let deadline = Instant::now() + FRAME;
        loop {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            if event::poll(deadline - now)? {
                if let Event::Key(key) = event::read()? {
                    if key.kind != KeyEventKind::Press {
                        continue;
                    }
                    match key.code {
                        KeyCode::Esc => return Ok(()),
                        KeyCode::Char('e') | KeyCode::Char('E') => return Ok(()),
                        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            return Ok(())
                        }
                        KeyCode::Char(' ') | KeyCode::Up => game.press_jump(),
                        _ => {}
                    }
                }
            }
        }

        game.update();
        draw(&mut out, &game, &mut last_size)?;
    }
}
