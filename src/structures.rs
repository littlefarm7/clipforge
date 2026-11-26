//! Estruturas de edição: modelos de corte inspirados em canais profissionais.
//!
//! Cada estrutura define como o clipe é montado (cortes, enquadramento, legendas,
//! títulos, música) e quais opções a pessoa pode ajustar.

use serde::{Deserialize, Serialize};

/// Descrição de uma estrutura para a interface.
#[derive(Debug, Clone, Serialize)]
pub struct Info {
    pub id: &'static str,
    pub name: &'static str,
    /// Uma linha que resume o estilo
    pub tagline: &'static str,
    /// Para que tipo de vídeo ela funciona melhor
    pub best_for: &'static str,
    /// O que a edição faz, passo a passo
    pub steps: &'static [&'static str],
    /// Opções de `StyleOpts` que fazem sentido nesta estrutura
    pub controls: &'static [&'static str],
}

pub const ALL: &[Info] = &[
    Info {
        id: "classic",
        name: "Clássico",
        tagline: "Rosto no centro e legenda karaokê",
        best_for: "Qualquer vídeo",
        steps: &[
            "Segue quem fala, junta duas pessoas ou divide a tela sozinho",
            "Legenda karaokê com a palavra falada em destaque",
        ],
        controls: &[],
    },
    Info {
        id: "turbo",
        name: "Podcast Turbo",
        tagline: "Cortes rápidos, sem pausas, legenda colorida por pessoa",
        best_for: "Podcasts e entrevistas com várias pessoas",
        steps: &[
            "Remove pausas e respirações: uma fala emenda na outra",
            "Câmera em quem fala, com planos de reação em quem ri ou escuta",
            "Zoom alternado a cada corte e aproximação nas frases fortes",
            "Legenda em frases curtas com uma cor para cada pessoa",
            "Palavrões censurados no estilo c4r4lh0",
            "Áudio alto e comprimido, pronto para o celular",
        ],
        controls: &[
            "cut_pauses",
            "reactions",
            "emphasis_zoom",
            "speaker_colors",
            "censor",
            "sfx",
            "music",
        ],
    },
    Info {
        id: "conversa",
        name: "Conversa",
        tagline: "Ritmo calmo, risadas com espaço e legenda de frase inteira",
        best_for: "Bate-papo, humor e histórias contadas com calma",
        steps: &[
            "Mantém as pausas: a piada tem tempo de acontecer",
            "Corta para quem fala e segura a câmera em quem está rindo",
            "Legenda de frase inteira, em maiúsculas, alinhada à esquerda",
            "Áudio no volume padrão das plataformas",
        ],
        controls: &["reactions", "cut_pauses", "censor", "music"],
    },
    Info {
        id: "react_split",
        name: "React Dividido",
        tagline: "Webcam em cima, jogo embaixo e título fixo",
        best_for: "Lives e gameplay com a webcam no canto da tela",
        steps: &[
            "Encontra a webcam do streamer e coloca no terço de cima",
            "O jogo ocupa os dois terços de baixo, de ponta a ponta",
            "Título fixo numa faixa branca entre as duas partes",
            "Legenda karaokê de 2 a 3 palavras com destaque amarelo",
        ],
        controls: &["title", "cut_pauses", "censor", "music"],
    },
    Info {
        id: "react_destaque",
        name: "React Destaque",
        tagline: "A tela muda: webcam grande na reação, jogo grande na ação",
        best_for: "Lives e gameplay com momentos de reação",
        steps: &[
            "Fundo desfocado e o jogo inteiro numa faixa no meio",
            "Quando o streamer fala, a webcam aparece grande em cima",
            "Na ação, a webcam vira uma janelinha no canto do jogo",
            "Título nos primeiros 3 segundos e legenda de uma palavra gigante",
        ],
        controls: &["title", "name_tag", "cut_pauses", "censor", "sfx", "music"],
    },
    Info {
        id: "cinema",
        name: "Cinemático",
        tagline: "Close no rosto, cor tratada e legenda que cresce ao lado",
        best_for: "Cenas de séries, filmes e esquetes",
        steps: &[
            "Recorte fechado no rosto de quem fala",
            "Cor tratada: mais contraste, cantos escuros e nitidez",
            "Legenda empilhada: uma palavra por linha, somando a frase",
            "Palavras-chave em azul-claro",
        ],
        controls: &["grade", "emphasis_zoom", "cut_pauses", "sfx", "music"],
    },
    Info {
        id: "meme",
        name: "Meme",
        tagline: "Texto de contexto em cima e o vídeo embaixo",
        best_for: "Momentos engraçados curtos",
        steps: &[
            "A IA escreve o texto do meme (ex.: \"Minha mãe: … / Eu:\")",
            "Caixa branca com o texto e o vídeo inteiro logo abaixo",
            "Clipes curtos, de 8 a 22 segundos",
        ],
        controls: &["captions", "music"],
    },
    Info {
        id: "top",
        name: "Top Momentos",
        tagline: "Lista numerada que se completa a cada momento",
        best_for: "Compilações de lives, podcasts e vídeos longos",
        steps: &[
            "Junta vários momentos curtos num clipe só",
            "Título fixo e lista numerada digitada à esquerda",
            "Tela preta rápida entre um momento e outro",
            "Música de fundo que abaixa quando alguém fala",
        ],
        controls: &["list_title", "items", "captions", "sfx", "music"],
    },
];

pub fn info(id: &str) -> Option<&'static Info> {
    ALL.iter().find(|i| i.id == id)
}

/// Opções de uma estrutura. Cada estrutura usa só as que aparecem em `controls`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct StyleOpts {
    /// Arquivo da biblioteca de músicas ("" = sem música)
    pub music: String,
    /// Volume da música (0..1) quando ninguém fala; abaixa sozinho sob a fala
    pub music_volume: f64,
    /// Remove pausas entre as falas
    pub cut_pauses: bool,
    /// Planos de reação (câmera em quem escuta ou ri)
    pub reactions: bool,
    /// Zoom alternado nos cortes e aproximação nas frases fortes
    pub emphasis_zoom: bool,
    /// Uma cor de legenda para cada pessoa
    pub speaker_colors: bool,
    /// Palavrões escritos com números (c4r4lh0)
    pub censor: bool,
    /// Título do clipe na tela
    pub title: bool,
    /// Cor tratada (contraste, vinheta, nitidez)
    pub grade: bool,
    /// Legendas da fala
    pub captions: bool,
    /// Efeitos sonoros (whoosh) nos zooms e transições
    pub sfx: bool,
    /// Nome mostrado embaixo da webcam
    pub name_tag: String,
    /// Título da lista (Top Momentos)
    pub list_title: String,
    /// Momentos por lista (Top Momentos)
    pub items: u32,
}

impl Default for StyleOpts {
    fn default() -> Self {
        Self {
            music: String::new(),
            music_volume: 0.22,
            cut_pauses: true,
            reactions: true,
            emphasis_zoom: true,
            speaker_colors: true,
            censor: true,
            title: true,
            grade: true,
            captions: true,
            sfx: true,
            name_tag: String::new(),
            list_title: String::new(),
            items: 4,
        }
    }
}

/// Opções padrão de cada estrutura.
pub fn defaults(id: &str) -> StyleOpts {
    let d = StyleOpts::default();
    match id {
        "conversa" => StyleOpts {
            cut_pauses: false,
            emphasis_zoom: false,
            ..d
        },
        "cinema" => StyleOpts {
            cut_pauses: false,
            reactions: false,
            ..d
        },
        "meme" => StyleOpts {
            cut_pauses: false,
            reactions: false,
            emphasis_zoom: false,
            ..d
        },
        "react_split" | "react_destaque" => StyleOpts {
            cut_pauses: false,
            reactions: false,
            ..d
        },
        "top" => StyleOpts {
            music_volume: 0.18,
            captions: false,
            ..d
        },
        _ => d,
    }
}

/// Opções efetivas: as que a estrutura mostra vêm das escolhas da pessoa; as outras ficam
/// no padrão da estrutura.
pub fn effective(id: &str, saved: &StyleOpts) -> StyleOpts {
    let mut o = defaults(id);
    let shown = info(id).map(|i| i.controls).unwrap_or(&[]);
    let has = |c: &str| shown.contains(&c);
    if has("music") {
        o.music = saved.music.clone();
        o.music_volume = saved.music_volume;
    }
    if has("cut_pauses") {
        o.cut_pauses = saved.cut_pauses;
    }
    if has("reactions") {
        o.reactions = saved.reactions;
    }
    if has("emphasis_zoom") {
        o.emphasis_zoom = saved.emphasis_zoom;
    }
    if has("speaker_colors") {
        o.speaker_colors = saved.speaker_colors;
    }
    if has("censor") {
        o.censor = saved.censor;
    }
    if has("title") {
        o.title = saved.title;
    }
    if has("grade") {
        o.grade = saved.grade;
    }
    if has("captions") {
        o.captions = saved.captions;
    }
    if has("sfx") {
        o.sfx = saved.sfx;
    }
    if has("name_tag") {
        o.name_tag = saved.name_tag.clone();
    }
    if has("list_title") {
        o.list_title = saved.list_title.clone();
    }
    if has("items") {
        o.items = saved.items.clamp(2, 8);
    }
    o
}

/// Duração (mín., máx.) de cada clipe, ou de cada momento no Top Momentos.
/// `None` usa a duração escolhida nos ajustes.
pub fn length_override(id: &str) -> Option<(f64, f64)> {
    match id {
        "meme" => Some((8.0, 22.0)),
        "top" => Some((4.0, 9.0)),
        _ => None,
    }
}

/// Volume final (LUFS) de cada estrutura.
pub fn loudness(id: &str) -> f64 {
    match id {
        "turbo" => -10.0,
        "top" => -10.0,
        "conversa" => -14.0,
        _ => -12.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn todas_as_estruturas_tem_padroes_coerentes() {
        for s in ALL {
            assert!(info(s.id).is_some());
            let d = defaults(s.id);
            assert!(d.items >= 2 && d.music_volume > 0.0);
        }
        assert!(!defaults("conversa").cut_pauses);
        assert!(defaults("turbo").cut_pauses);
        assert!(!defaults("meme").cut_pauses);
        // Opções escondidas numa estrutura ficam no padrão dela
        let saved = StyleOpts {
            cut_pauses: true,
            ..StyleOpts::default()
        };
        assert!(!effective("meme", &saved).cut_pauses);
        assert_eq!(length_override("meme"), Some((8.0, 22.0)));
    }
}
