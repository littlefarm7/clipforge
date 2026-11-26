//! Legendas e textos na tela (ASS) de cada estrutura de edição.
//!
//! Todas as funções recebem as palavras já na linha do tempo do clipe final (depois de
//! remover pausas e juntar momentos) e devolvem eventos ASS prontos.

use crate::config::Config;
use crate::render::{ass_color, ass_escape, ass_time};
use std::fmt::Write as _;

/// Uma palavra no clipe final.
#[derive(Debug, Clone, PartialEq)]
pub struct OWord {
    pub start: f64,
    pub end: f64,
    pub text: String,
    /// Quem falou (cor da legenda), quando se sabe
    pub speaker: Option<usize>,
}

/// Cores de legenda por pessoa (amarelo, vermelho, verde, laranja, azul-claro).
pub const PALETTE: [&str; 5] = ["#FFE14D", "#FF4D4D", "#5CFF7A", "#FF9F1C", "#4DE1FF"];
const RED: &str = "#FF3B3B";
const YELLOW: &str = "#FFE14D";
const CYAN: &str = "#4DE1FF";

/// Geometria e textos do clipe que as legendas precisam conhecer.
pub struct Scene<'a> {
    pub cfg: &'a Config,
    pub w: u32,
    pub h: u32,
    pub dur: f64,
}

impl Scene<'_> {
    fn k(&self) -> f64 {
        self.w.min(self.h) as f64 / 1080.0
    }
    fn px(&self, v: f64) -> i64 {
        (v * self.k()).round() as i64
    }
}

/// Linha "Style:" do ASS.
#[allow(clippy::too_many_arguments)]
fn style(
    name: &str,
    font: &str,
    size: i64,
    primary: &str,
    outline_col: &str,
    back: &str,
    bold: bool,
    italic: bool,
    boxed: bool,
    outline: i64,
    shadow: i64,
    align: u8,
    margins: (i64, i64, i64),
) -> String {
    format!(
        "Style: {name},{font},{size},{primary},&H000000FF&,{outline_col},{back},{},{},0,0,100,100,0,0,{},{outline},{shadow},{align},{},{},{},1\n",
        if bold { -1 } else { 0 },
        if italic { -1 } else { 0 },
        if boxed { 3 } else { 1 },
        margins.0,
        margins.1,
        margins.2,
    )
}

fn header(sc: &Scene, styles: &str) -> String {
    format!(
        "[Script Info]\nScriptType: v4.00+\nPlayResX: {}\nPlayResY: {}\nWrapStyle: 2\nScaledBorderAndShadow: yes\n\n\
[V4+ Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\n\
{styles}\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n",
        sc.w, sc.h
    )
}

fn event(out: &mut String, layer: u8, t0: f64, t1: f64, style: &str, text: &str) {
    if t1 - t0 < 0.01 {
        return;
    }
    let _ = writeln!(
        out,
        "Dialogue: {layer},{},{},{style},,0,0,0,,{text}",
        ass_time(t0),
        ass_time(t1)
    );
}

/// Marca d'água semitransparente com o @ do perfil (se configurada).
fn watermark(sc: &Scene, y_frac: Option<f64>) -> (String, String) {
    let Some(text) = sc.cfg.watermark_text() else {
        return (String::new(), String::new());
    };
    let st = style(
        "Marca",
        "Poppins",
        sc.px(46.0),
        "&H8CFFFFFF&",
        "&HC8000000&",
        "&HFF000000&",
        true,
        false,
        false,
        sc.px(2.0).max(1),
        0,
        5,
        (0, 0, 0),
    );
    let y = y_frac.unwrap_or(match sc.cfg.watermark_position.as_str() {
        "middle" => 0.58,
        "bottom" => 0.92,
        _ => 0.07,
    }) * sc.h as f64;
    let mut ev = String::new();
    event(
        &mut ev,
        9,
        0.0,
        sc.dur + 1.0,
        "Marca",
        &format!("{{\\pos({},{})}}{}", sc.w / 2, y.round(), ass_escape(&text)),
    );
    (st, ev)
}

// ---------------------------------------------------------------------------
// Utilidades de texto
// ---------------------------------------------------------------------------

/// Palavrões (sem acento, minúsculas) que são escritos com números.
const PROFANITY: &[&str] = &[
    "porra",
    "caralho",
    "caralhos",
    "merda",
    "puta",
    "puto",
    "putaria",
    "foda",
    "fodase",
    "foder",
    "fodido",
    "fudido",
    "fodeu",
    "fudeu",
    "buceta",
    "bosta",
    "cacete",
    "arrombado",
    "piroca",
    "viado",
    "cu",
    "fuck",
    "fucking",
    "shit",
    "bitch",
    "asshole",
];

fn plain(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .map(|c| match c {
            'á' | 'à' | 'â' | 'ã' | 'Á' | 'À' | 'Â' | 'Ã' => 'a',
            'é' | 'ê' | 'É' | 'Ê' => 'e',
            'í' | 'Í' => 'i',
            'ó' | 'ô' | 'õ' | 'Ó' | 'Ô' | 'Õ' => 'o',
            'ú' | 'Ú' => 'u',
            'ç' | 'Ç' => 'c',
            c => c.to_ascii_lowercase(),
        })
        .collect()
}

pub fn is_profane(word: &str) -> bool {
    PROFANITY.contains(&plain(word).as_str())
}

/// "caralho" → "c4r4lh0" (mantém pontuação e maiúsculas).
pub fn leet(word: &str) -> String {
    word.chars()
        .map(|c| match c {
            'a' | 'A' | 'á' | 'ã' | 'â' | 'Á' | 'Ã' | 'Â' => '4',
            'e' | 'E' | 'é' | 'ê' | 'É' | 'Ê' => '3',
            'i' | 'I' | 'í' | 'Í' => '1',
            'o' | 'O' | 'ó' | 'õ' | 'ô' | 'Ó' | 'Õ' | 'Ô' => '0',
            c => c,
        })
        .collect()
}

fn ends_sentence(w: &str) -> bool {
    w.ends_with(['.', '?', '!', '…'])
}

/// Agrupa palavras em blocos curtos (por quantidade, tamanho, pontuação e pausas).
fn groups(words: &[OWord], max_words: usize, max_chars: usize, max_gap: f64) -> Vec<Vec<OWord>> {
    let mut out: Vec<Vec<OWord>> = Vec::new();
    let mut cur: Vec<OWord> = Vec::new();
    for (i, w) in words.iter().enumerate() {
        cur.push(w.clone());
        let next = words.get(i + 1);
        let gap = next.map(|n| n.start - w.end).unwrap_or(99.0);
        let speaker_change = next.is_some_and(|n| n.speaker != w.speaker);
        let chars: usize = cur.iter().map(|x| x.text.chars().count() + 1).sum();
        let next_chars = next.map(|n| n.text.chars().count()).unwrap_or(0);
        if cur.len() >= max_words
            || w.text.ends_with(['.', '?', '!', ',', ';', ':', '…'])
            || gap > max_gap
            || speaker_change
            || chars + next_chars > max_chars
        {
            out.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Quebra o texto em linhas de até `max` caracteres (sem cortar palavras).
pub fn wrap(text: &str, max: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    for word in text.split_whitespace() {
        if !cur.is_empty() && cur.chars().count() + 1 + word.chars().count() > max {
            lines.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(word);
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    lines
}

/// Divide em duas linhas equilibradas (índice da primeira palavra da 2ª linha).
fn balance(words: &[String]) -> Option<usize> {
    let total: usize = words.iter().map(|w| w.chars().count() + 1).sum();
    if words.len() < 2 || total <= 14 {
        return None;
    }
    let mut acc = 0;
    let mut best = (usize::MAX, 1);
    for (i, w) in words.iter().enumerate().take(words.len() - 1) {
        acc += w.chars().count() + 1;
        let diff = (acc as i64 - (total - acc) as i64).unsigned_abs() as usize;
        if diff < best.0 {
            best = (diff, i + 1);
        }
    }
    Some(best.1)
}

/// Texto de uma palavra com censura (cor vermelha) e cor de volta à do bloco.
fn word_text(w: &str, censor: bool, back: &str, upper: bool) -> String {
    let t = if upper {
        w.to_uppercase()
    } else {
        w.to_string()
    };
    if censor && is_profane(w) {
        format!(
            "{{\\c{}}}{}{{\\c{}}}",
            ass_color(RED),
            ass_escape(&leet(&t)),
            ass_color(back)
        )
    } else {
        ass_escape(&t)
    }
}

/// Cor de cada pessoa, na ordem em que aparecem.
fn speaker_colors(words: &[OWord]) -> impl Fn(Option<usize>) -> &'static str {
    let mut order: Vec<usize> = Vec::new();
    for w in words {
        if let Some(s) = w.speaker
            && !order.contains(&s)
        {
            order.push(s);
        }
    }
    move |s: Option<usize>| {
        s.and_then(|s| order.iter().position(|x| *x == s))
            .map(|i| PALETTE[i % PALETTE.len()])
            .unwrap_or(PALETTE[0])
    }
}

/// Fim de exibição de um bloco: até o próximo começar (no máximo 0,6 s depois da fala).
fn hold(g: &[OWord], next: Option<&Vec<OWord>>, dur: f64) -> f64 {
    let end = g.last().map(|w| w.end).unwrap_or(0.0) + 0.6;
    next.map(|n| n[0].start).unwrap_or(dur).min(end).min(dur)
}

// ---------------------------------------------------------------------------
// Estilos
// ---------------------------------------------------------------------------

/// Podcast Turbo: frases curtas em 2 linhas, cor por pessoa, censura, "…" nas frases cortadas.
pub fn turbo(sc: &Scene, words: &[OWord], colors: bool, censor: bool, y_frac: f64) -> String {
    let mut styles = style(
        "Frase",
        "Poppins ExtraBold",
        sc.px(130.0),
        &ass_color(PALETTE[0]),
        "&H00000000&",
        "&H80000000&",
        false,
        true,
        false,
        sc.px(6.0),
        sc.px(2.0),
        2,
        (sc.px(70.0), sc.px(70.0), (sc.h as f64 * y_frac) as i64),
    );
    let (wst, wev) = watermark(sc, None);
    styles.push_str(&wst);
    let mut s = header(sc, &styles);
    let color_of = speaker_colors(words);
    let gs = groups(words, 4, 22, 0.45);
    for (gi, g) in gs.iter().enumerate() {
        let next = gs.get(gi + 1);
        let color = if colors {
            color_of(g[0].speaker)
        } else {
            PALETTE[0]
        };
        let mut texts: Vec<String> = g
            .iter()
            .map(|w| word_text(&w.text, censor, color, false))
            .collect();
        // Frase que continua no próximo bloco termina com reticências
        let last = &g[g.len() - 1].text;
        if !last.ends_with(['.', '?', '!', ',', ';', ':', '…'])
            && next.is_some_and(|n| n[0].start - g[g.len() - 1].end < 0.7)
            && let Some(t) = texts.last_mut()
        {
            t.push('…');
        }
        let raw: Vec<String> = g.iter().map(|w| w.text.clone()).collect();
        let body = match balance(&raw) {
            Some(i) => format!("{}\\N{}", texts[..i].join(" "), texts[i..].join(" ")),
            None => texts.join(" "),
        };
        let t1 = hold(g, next, sc.dur);
        event(
            &mut s,
            0,
            g[0].start,
            t1,
            "Frase",
            &format!(
                "{{\\c{}\\fscx86\\fscy86\\t(0,90,\\fscx100\\fscy100)}}{body}",
                ass_color(color)
            ),
        );
    }
    s.push_str(&wev);
    s
}

/// Conversa: frase inteira em maiúsculas, branca, alinhada à esquerda.
pub fn conversa(sc: &Scene, words: &[OWord], censor: bool) -> String {
    let mut styles = style(
        "Frase",
        "Poppins ExtraBold",
        sc.px(112.0),
        "&H00FFFFFF&",
        "&H00000000&",
        "&H90000000&",
        false,
        false,
        false,
        sc.px(5.0),
        sc.px(3.0),
        1,
        (sc.px(78.0), sc.px(78.0), (sc.h as f64 * 0.34) as i64),
    );
    let (wst, wev) = watermark(sc, None);
    styles.push_str(&wst);
    let mut s = header(sc, &styles);
    let gs = groups(words, 9, 44, 0.8);
    for (gi, g) in gs.iter().enumerate() {
        let next = gs.get(gi + 1);
        let text = g
            .iter()
            .map(|w| w.text.to_uppercase())
            .collect::<Vec<_>>()
            .join(" ");
        // Quebra em linhas curtas; a censura é aplicada palavra a palavra
        let lines: Vec<String> = wrap(&text, 13)
            .iter()
            .map(|l| {
                l.split(' ')
                    .map(|w| word_text(w, censor, "#FFFFFF", true))
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect();
        event(
            &mut s,
            0,
            g[0].start,
            hold(g, next, sc.dur),
            "Frase",
            &lines.join("\\N"),
        );
    }
    s.push_str(&wev);
    s
}

/// Karaokê de 2–3 palavras com destaque amarelo (React Dividido).
/// `y` é o centro vertical da legenda; `title` aparece numa faixa branca em `title_y`.
pub fn karaoke(
    sc: &Scene,
    words: &[OWord],
    censor: bool,
    y: f64,
    title: Option<(&str, f64)>,
) -> String {
    let mut styles = style(
        "Kara",
        "Archivo Black",
        sc.px(96.0),
        "&H00FFFFFF&",
        "&H00000000&",
        "&H80000000&",
        false,
        false,
        false,
        sc.px(6.0),
        sc.px(2.0),
        5,
        (0, 0, 0),
    );
    styles.push_str(&style(
        "Faixa",
        "Poppins ExtraBold",
        sc.px(66.0),
        "&H00000000&",
        "&H00FFFFFF&",
        "&H00FFFFFF&",
        false,
        false,
        true,
        sc.px(16.0),
        0,
        5,
        (0, 0, 0),
    ));
    let (wst, wev) = watermark(sc, None);
    styles.push_str(&wst);
    let mut s = header(sc, &styles);
    if let Some((t, ty)) = title {
        let lines = wrap(&t.to_uppercase(), 26);
        event(
            &mut s,
            2,
            0.0,
            sc.dur + 1.0,
            "Faixa",
            &format!(
                "{{\\pos({},{})}}{}",
                sc.w / 2,
                ty.round(),
                lines
                    .iter()
                    .map(|l| ass_escape(l))
                    .collect::<Vec<_>>()
                    .join("\\N")
            ),
        );
    }
    let gs = groups(words, 3, 15, 0.5);
    let hl = ass_color(YELLOW);
    for (gi, g) in gs.iter().enumerate() {
        let next = gs.get(gi + 1);
        let g_end = hold(g, next, sc.dur);
        for (i, w) in g.iter().enumerate() {
            let t0 = w.start;
            let t1 = g.get(i + 1).map(|n| n.start).unwrap_or(g_end);
            let text = g
                .iter()
                .enumerate()
                .map(|(j, x)| {
                    let t = word_text(&x.text, censor, "#FFFFFF", true);
                    if j == i {
                        format!("{{\\c{hl}\\fscx112\\fscy112}}{t}{{\\r}}")
                    } else {
                        t
                    }
                })
                .collect::<Vec<_>>()
                .join(" ");
            event(
                &mut s,
                0,
                t0,
                t1,
                "Kara",
                &format!("{{\\pos({},{})}}{text}", sc.w / 2, y.round()),
            );
        }
    }
    s.push_str(&wev);
    s
}

/// Nome embaixo da webcam: texto, altura e intervalos em que a webcam está grande.
pub type NameTag<'a> = (&'a str, f64, &'a [(f64, f64)]);

/// React Destaque: uma palavra gigante por vez (branco e amarelo alternados), título nos
/// primeiros segundos e o nome embaixo da webcam enquanto ela está grande.
#[allow(clippy::too_many_arguments)]
pub fn giant(
    sc: &Scene,
    words: &[OWord],
    censor: bool,
    y: f64,
    title: Option<&str>,
    name_tag: Option<NameTag>,
) -> String {
    let mut styles = style(
        "Giga",
        "Poppins ExtraBold",
        sc.px(190.0),
        "&H00FFFFFF&",
        "&H00000000&",
        "&H80000000&",
        false,
        true,
        false,
        sc.px(7.0),
        sc.px(3.0),
        5,
        (0, 0, 0),
    );
    styles.push_str(&style(
        "Titulo",
        "Archivo Black",
        sc.px(72.0),
        "&H00FFFFFF&",
        "&H00000000&",
        "&H80000000&",
        false,
        false,
        false,
        sc.px(5.0),
        sc.px(2.0),
        8,
        (sc.px(60.0), sc.px(60.0), (sc.h as f64 * 0.025) as i64),
    ));
    styles.push_str(&style(
        "Nome",
        "Poppins ExtraBold",
        sc.px(46.0),
        "&H00000000&",
        "&H00FFFFFF&",
        "&H00FFFFFF&",
        false,
        false,
        true,
        sc.px(10.0),
        0,
        5,
        (0, 0, 0),
    ));
    let (wst, wev) = watermark(sc, Some(0.95));
    styles.push_str(&wst);
    let mut s = header(sc, &styles);
    if let Some(t) = title {
        // A palavra mais longa do título fica amarela
        let up = t.to_uppercase();
        let key = up
            .split_whitespace()
            .max_by_key(|w| w.chars().count())
            .unwrap_or("")
            .to_string();
        let mut done = false;
        let lines: Vec<String> = wrap(&up, 20)
            .iter()
            .map(|l| {
                l.split(' ')
                    .map(|w| {
                        if !done && w == key {
                            done = true;
                            format!(
                                "{{\\c{}}}{}{{\\c&H00FFFFFF&}}",
                                ass_color(YELLOW),
                                ass_escape(w)
                            )
                        } else {
                            ass_escape(w)
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect();
        event(
            &mut s,
            3,
            0.0,
            3.2f64.min(sc.dur),
            "Titulo",
            &format!("{{\\fad(0,250)}}{}", lines.join("\\N")),
        );
    }
    if let Some((name, ny, spans)) = name_tag
        && !name.trim().is_empty()
    {
        for (a, b) in spans {
            event(
                &mut s,
                4,
                *a,
                *b,
                "Nome",
                &format!(
                    "{{\\pos({},{})}}{}",
                    sc.w / 2,
                    ny.round(),
                    ass_escape(&name.trim().to_uppercase())
                ),
            );
        }
    }
    // Palavras curtas seguidas viram um bloco só ("A RUA")
    let mut blocks: Vec<Vec<OWord>> = Vec::new();
    for w in words {
        match blocks.last_mut() {
            Some(b)
                if b.iter().map(|x| x.text.chars().count() + 1).sum::<usize>()
                    + w.text.chars().count()
                    <= 8
                    && w.start - b[b.len() - 1].end < 0.25
                    && !ends_sentence(&b[b.len() - 1].text) =>
            {
                b.push(w.clone())
            }
            _ => blocks.push(vec![w.clone()]),
        }
    }
    for (i, b) in blocks.iter().enumerate() {
        let color = if i % 2 == 0 { "#FFFFFF" } else { YELLOW };
        let text = b
            .iter()
            .map(|w| {
                word_text(
                    w.text.trim_end_matches([',', ';', ':']),
                    censor,
                    color,
                    true,
                )
            })
            .collect::<Vec<_>>()
            .join(" ");
        let t1 = blocks
            .get(i + 1)
            .map(|n| n[0].start)
            .unwrap_or(sc.dur)
            .min(b[b.len() - 1].end + 0.45);
        event(
            &mut s,
            0,
            b[0].start,
            t1,
            "Giga",
            &format!(
                "{{\\pos({},{})\\c{}\\fscx125\\fscy125\\t(0,80,\\fscx100\\fscy100)}}{text}",
                sc.w / 2,
                y.round(),
                ass_color(color)
            ),
        );
    }
    s.push_str(&wev);
    s
}

/// Cinemático: legenda empilhada ao lado do rosto (uma palavra por linha, somando a frase),
/// palavras-chave em azul-claro.
pub fn stacked(sc: &Scene, words: &[OWord], censor: bool) -> String {
    let mut styles = style(
        "Pilha",
        "Poppins ExtraBold",
        sc.px(132.0),
        "&H00FFFFFF&",
        "&H00000000&",
        "&H64000000&",
        false,
        false,
        false,
        sc.px(3.0),
        sc.px(5.0),
        7,
        (0, 0, 0),
    );
    let (wst, wev) = watermark(sc, Some(0.9));
    styles.push_str(&wst);
    let mut s = header(sc, &styles);
    let cyan = ass_color(CYAN);
    let gs = groups(words, 6, 40, 0.5);
    for (gi, g) in gs.iter().enumerate() {
        let next = gs.get(gi + 1);
        let g_end = hold(g, next, sc.dur);
        // Artigos e preposições curtas ficam na linha da palavra seguinte ("a casa")
        let mut lines: Vec<(usize, String)> = Vec::new(); // (índice da última palavra, texto)
        let mut pending = String::new();
        for (i, w) in g.iter().enumerate() {
            let last = i + 1 == g.len();
            let key = w.text.chars().filter(|c| c.is_alphanumeric()).count() >= 7
                || (last && w.text.ends_with(['?', '!']));
            let mut t = word_text(&w.text, censor, "#FFFFFF", false);
            if key && !(censor && is_profane(&w.text)) {
                t = format!("{{\\c{cyan}}}{t}{{\\c&H00FFFFFF&}}");
            }
            if w.text.chars().count() <= 2 && !last {
                pending.push_str(&t);
                pending.push(' ');
                continue;
            }
            lines.push((i, format!("{pending}{t}")));
            pending.clear();
        }
        for (li, (wi, _)) in lines.iter().enumerate() {
            let t0 = if li == 0 {
                g[0].start
            } else {
                g[lines[li - 1].0 + 1].start
            };
            let t1 = lines
                .get(li + 1)
                .map(|(_, _)| g[*wi + 1].start)
                .unwrap_or(g_end);
            let body = lines[..=li]
                .iter()
                .map(|(_, t)| t.clone())
                .collect::<Vec<_>>()
                .join("\\N");
            event(
                &mut s,
                0,
                t0,
                t1,
                "Pilha",
                &format!(
                    "{{\\pos({},{})}}{body}",
                    sc.px(70.0),
                    (sc.h as f64 * 0.2).round()
                ),
            );
        }
    }
    s.push_str(&wev);
    s
}

/// Meme: texto de contexto preto sobre a caixa branca, @ no topo e legenda pequena opcional.
pub fn meme(
    sc: &Scene,
    context_lines: &[String],
    box_center_y: f64,
    header_y: Option<f64>,
    words: &[OWord],
    caption_y: f64,
) -> String {
    let mut styles = style(
        "Contexto",
        "Poppins ExtraBold",
        sc.px(80.0),
        "&H00000000&",
        "&H00FFFFFF&",
        "&HFF000000&",
        false,
        false,
        false,
        0,
        0,
        5,
        (0, 0, 0),
    );
    styles.push_str(&style(
        "Perfil",
        "Poppins ExtraBold",
        sc.px(52.0),
        "&H00FFFFFF&",
        "&H00000000&",
        "&HFF000000&",
        false,
        false,
        false,
        0,
        0,
        5,
        (0, 0, 0),
    ));
    styles.push_str(&style(
        "Mini",
        "Poppins",
        sc.px(62.0),
        &ass_color(YELLOW),
        "&H00000000&",
        "&H80000000&",
        true,
        false,
        false,
        sc.px(4.0),
        0,
        2,
        (sc.px(40.0), sc.px(40.0), 0),
    ));
    let mut s = header(sc, &styles);
    let body = context_lines
        .iter()
        .map(|l| ass_escape(l))
        .collect::<Vec<_>>()
        .join("\\N");
    event(
        &mut s,
        1,
        0.0,
        sc.dur + 1.0,
        "Contexto",
        &format!("{{\\pos({},{})}}{body}", sc.w / 2, box_center_y.round()),
    );
    if let (Some(hy), Some(handle)) = (header_y, sc.cfg.watermark_text()) {
        event(
            &mut s,
            1,
            0.0,
            sc.dur + 1.0,
            "Perfil",
            &format!(
                "{{\\pos({},{})}}{}",
                sc.w / 2,
                hy.round(),
                ass_escape(&handle)
            ),
        );
    }
    let gs = groups(words, 6, 30, 0.6);
    for (gi, g) in gs.iter().enumerate() {
        let text = g
            .iter()
            .map(|w| ass_escape(&w.text.to_lowercase()))
            .collect::<Vec<_>>()
            .join(" ");
        event(
            &mut s,
            0,
            g[0].start,
            hold(g, gs.get(gi + 1), sc.dur),
            "Mini",
            &format!("{{\\pos({},{})}}{text}", sc.w / 2, caption_y.round()),
        );
    }
    s
}

/// Top Momentos: título fixo e lista numerada digitada à esquerda.
/// `items` = (início do momento no clipe, rótulo).
pub fn top_list(sc: &Scene, title: &str, items: &[(f64, String)], words: &[OWord]) -> String {
    let mut styles = style(
        "Titulo",
        "Archivo Black",
        sc.px(78.0),
        "&H00FFFFFF&",
        "&H00000000&",
        "&H80000000&",
        false,
        false,
        false,
        sc.px(6.0),
        sc.px(3.0),
        8,
        (sc.px(50.0), sc.px(50.0), (sc.h as f64 * 0.045) as i64),
    );
    styles.push_str(&style(
        "Item",
        "Poppins ExtraBold",
        sc.px(70.0),
        "&H00FFFFFF&",
        "&H00000000&",
        "&H80000000&",
        false,
        false,
        false,
        sc.px(5.0),
        sc.px(2.0),
        7,
        (0, 0, 0),
    ));
    if !words.is_empty() {
        styles.push_str(&style(
            "Frase",
            "Poppins ExtraBold",
            sc.px(100.0),
            &ass_color(YELLOW),
            "&H00000000&",
            "&H80000000&",
            false,
            true,
            false,
            sc.px(5.0),
            sc.px(2.0),
            2,
            (sc.px(70.0), sc.px(70.0), (sc.h as f64 * 0.2) as i64),
        ));
    }
    let (wst, wev) = watermark(sc, Some(0.95));
    styles.push_str(&wst);
    let mut s = header(sc, &styles);
    let t_lines = wrap(&title.to_uppercase(), 18);
    let title_text = t_lines
        .iter()
        .enumerate()
        .map(|(i, l)| {
            if i == t_lines.len() - 1 && t_lines.len() > 1 {
                format!(
                    "{{\\c{}}}{}{{\\c&H00FFFFFF&}}",
                    ass_color(YELLOW),
                    ass_escape(l)
                )
            } else {
                ass_escape(l)
            }
        })
        .collect::<Vec<_>>()
        .join("\\N");
    event(&mut s, 2, 0.0, sc.dur + 1.0, "Titulo", &title_text);
    let x = sc.px(54.0);
    let y0 = sc.h as f64 * 0.27;
    let step = sc.px(92.0) as f64;
    for (i, (t_start, label)) in items.iter().enumerate() {
        let num = format!(
            "{{\\c{}}}{}.{{\\c&H00FFFFFF&}} ",
            ass_color(PALETTE[i % PALETTE.len()]),
            i + 1
        );
        let pos = format!("{{\\pos({x},{})}}", (y0 + step * i as f64).round());
        let label: Vec<char> = label.chars().collect();
        // Antes do momento: só o número
        event(&mut s, 1, 0.0, *t_start, "Item", &format!("{pos}{num}"));
        // Digitação: uma letra a cada 35 ms
        let per = 0.035;
        for k in 1..=label.len() {
            let a = t_start + per * (k - 1) as f64;
            let b = if k == label.len() {
                sc.dur + 1.0
            } else {
                t_start + per * k as f64
            };
            let typed: String = label[..k].iter().collect();
            event(
                &mut s,
                1,
                a,
                b,
                "Item",
                &format!("{pos}{num}{}", ass_escape(&typed)),
            );
        }
        if label.is_empty() {
            event(
                &mut s,
                1,
                *t_start,
                sc.dur + 1.0,
                "Item",
                &format!("{pos}{num}"),
            );
        }
    }
    if !words.is_empty() {
        let gs = groups(words, 5, 26, 0.45);
        for (gi, g) in gs.iter().enumerate() {
            let text = g
                .iter()
                .map(|w| ass_escape(&w.text))
                .collect::<Vec<_>>()
                .join(" ");
            event(
                &mut s,
                0,
                g[0].start,
                hold(g, gs.get(gi + 1), sc.dur),
                "Frase",
                &text,
            );
        }
    }
    s.push_str(&wev);
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(t: f64, text: &str, sp: Option<usize>) -> OWord {
        OWord {
            start: t,
            end: t + 0.3,
            text: text.into(),
            speaker: sp,
        }
    }

    fn scene(cfg: &Config) -> Scene<'_> {
        Scene {
            cfg,
            w: 1080,
            h: 1920,
            dur: 10.0,
        }
    }

    #[test]
    fn censura_com_numeros() {
        assert!(is_profane("Caralho!"));
        assert!(!is_profane("carro"));
        assert_eq!(leet("caralho"), "c4r4lh0");
        assert_eq!(leet("porra"), "p0rr4");
    }

    #[test]
    fn turbo_usa_cor_por_pessoa_e_reticencias() {
        let cfg = Config::default();
        let words = vec![
            w(0.0, "Tá", Some(3)),
            w(0.35, "rindo", Some(3)),
            w(0.7, "do", Some(3)),
            w(1.05, "quê?", Some(3)),
            w(1.6, "Esse", Some(7)),
            w(1.95, "porra", Some(7)),
            w(2.3, "de", Some(7)),
            w(2.65, "relógio", Some(7)),
            w(3.0, "caríssimo", Some(7)),
            w(3.35, "aqui", Some(7)),
        ];
        let ass = turbo(&scene(&cfg), &words, true, true, 0.3);
        assert!(ass.contains(&ass_color(PALETTE[0])));
        assert!(ass.contains(&ass_color(PALETTE[1])));
        assert!(ass.contains("p0rr4"));
        assert!(ass.contains('…'));
    }

    #[test]
    fn pilha_soma_palavras_linha_a_linha() {
        let cfg = Config::default();
        let words = vec![
            w(0.0, "Tomou", None),
            w(0.4, "cerveja", None),
            w(0.8, "hoje?", None),
        ];
        let ass = stacked(&scene(&cfg), &words, true);
        let lines: Vec<&str> = ass.lines().filter(|l| l.starts_with("Dialogue")).collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[2].matches("\\N").count() == 2);
        assert!(lines[1].contains(&ass_color(CYAN)));
    }

    #[test]
    fn lista_digita_os_rotulos() {
        let cfg = Config::default();
        let ass = top_list(
            &scene(&cfg),
            "Momentos do canal",
            &[(0.0, "Não sou eu".into()), (5.0, "Cabelo".into())],
            &[],
        );
        assert!(ass.contains("Não sou eu"));
        assert!(ass.contains("Cab"));
        assert!(ass.contains("2."));
    }

    #[test]
    fn quebra_de_linha() {
        assert_eq!(
            wrap("minha mãe disse que eu devia", 12),
            vec!["minha mãe", "disse que eu", "devia"]
        );
    }
}
