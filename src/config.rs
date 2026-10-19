use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    // Transcrição
    /// "local" (whisper.cpp) ou "api" (compatível com OpenAI: Groq, OpenAI...)
    pub stt_provider: String,
    pub whisper_model: String,
    /// "auto" ou código ISO (pt, en, es...)
    pub language: String,
    pub stt_api_base: String,
    pub stt_api_key: String,
    pub stt_api_model: String,

    // IA para escolher os melhores trechos
    /// "none" (heurística), "api" ou "local" (llama.cpp embutido)
    pub llm_provider: String,
    pub llm_api_base: String,
    pub llm_api_key: String,
    pub llm_api_model: String,
    pub llm_local_model: String,
    pub llm_ctx: u32,
    pub extra_prompt: String,

    // Clipes
    pub max_clips: u32,
    pub min_len: f64,
    pub max_len: f64,
    /// "9:16", "1:1", "4:5", "16:9"
    pub aspect: String,
    /// "face", "center", "blur"
    pub crop_mode: String,
    /// "1080" ou "720"
    pub quality: String,

    // Legendas
    pub subtitles: bool,
    pub sub_font: String,
    pub sub_size: u32,
    pub sub_color: String,
    pub sub_highlight: String,
    /// "bottom", "middle", "top"
    pub sub_position: String,
    pub sub_uppercase: bool,
    pub words_per_line: u32,

    /// 0 = automático
    pub threads: u32,
    /// Argumentos extras do yt-dlp (ex.: --cookies-from-browser firefox)
    pub ytdlp_args: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            stt_provider: "local".into(),
            whisper_model: "ggml-base.bin".into(),
            language: "auto".into(),
            stt_api_base: "https://api.groq.com/openai/v1".into(),
            stt_api_key: String::new(),
            stt_api_model: "whisper-large-v3-turbo".into(),
            llm_provider: "none".into(),
            llm_api_base: "https://api.groq.com/openai/v1".into(),
            llm_api_key: String::new(),
            llm_api_model: "llama-3.3-70b-versatile".into(),
            llm_local_model: String::new(),
            llm_ctx: 8192,
            extra_prompt: String::new(),
            max_clips: 6,
            min_len: 20.0,
            max_len: 60.0,
            aspect: "9:16".into(),
            crop_mode: "face".into(),
            quality: "1080".into(),
            subtitles: true,
            sub_font: "Anton".into(),
            sub_size: 90,
            sub_color: "#FFFFFF".into(),
            sub_highlight: "#FFE000".into(),
            sub_position: "bottom".into(),
            sub_uppercase: true,
            words_per_line: 3,
            threads: 0,
            ytdlp_args: String::new(),
        }
    }
}

impl Config {
    pub fn load() -> Self {
        std::fs::read_to_string(crate::paths::config_path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let p = crate::paths::config_path();
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::write(p, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    pub fn threads(&self) -> u32 {
        if self.threads > 0 {
            self.threads
        } else {
            std::thread::available_parallelism().map(|n| n.get() as u32).unwrap_or(4).clamp(1, 16)
        }
    }

    /// Tamanho de saída (largura, altura) conforme proporção e qualidade.
    pub fn out_size(&self) -> (u32, u32) {
        let base = if self.quality == "720" { 720 } else { 1080 };
        let even = |v: f64| ((v / 2.0).round() as u32) * 2;
        match self.aspect.as_str() {
            "1:1" => (base, base),
            "4:5" => (base, even(base as f64 * 1.25)),
            "16:9" => (even(base as f64 * 16.0 / 9.0), base),
            _ => (base, even(base as f64 * 16.0 / 9.0)),
        }
    }
}
