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
    /// Memória de contexto da IA local; 0 = automático (conforme a RAM)
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
    /// Estrutura de edição padrão dos novos projetos (ver `structures`)
    pub structure: String,
    /// Opções de cada estrutura (as que faltarem usam o padrão da estrutura)
    pub styles: std::collections::BTreeMap<String, crate::structures::StyleOpts>,

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

    // Marca d'água
    /// @ do perfil mostrado semitransparente no vídeo (vazio = sem marca d'água)
    pub watermark: String,
    /// "top", "middle" ou "bottom"
    pub watermark_position: String,

    /// Nível de qualidade escolhido no modo local ("fast", "balanced", "best")
    pub tier: String,
    /// 0 = automático
    pub threads: u32,
    /// Argumentos extras do yt-dlp (ex.: --cookies-from-browser firefox)
    pub ytdlp_args: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            stt_provider: "local".into(),
            whisper_model: "ggml-small.bin".into(),
            language: "auto".into(),
            stt_api_base: "https://api.groq.com/openai/v1".into(),
            stt_api_key: String::new(),
            stt_api_model: "whisper-large-v3-turbo".into(),
            llm_provider: "local".into(),
            llm_api_base: "https://api.groq.com/openai/v1".into(),
            llm_api_key: String::new(),
            llm_api_model: "llama-3.3-70b-versatile".into(),
            llm_local_model: "gemma-4-E4B-it-Q4_K_M.gguf".into(),
            llm_ctx: 0,
            extra_prompt: String::new(),
            max_clips: 6,
            min_len: 30.0,
            max_len: 60.0,
            aspect: "9:16".into(),
            crop_mode: "face".into(),
            quality: "1080".into(),
            structure: "turbo".into(),
            styles: Default::default(),
            subtitles: true,
            sub_font: "Anton".into(),
            sub_size: 90,
            sub_color: "#FFFFFF".into(),
            sub_highlight: "#FFE000".into(),
            sub_position: "bottom".into(),
            sub_uppercase: true,
            words_per_line: 3,
            watermark: String::new(),
            watermark_position: "top".into(),
            tier: String::new(),
            threads: 0,
            ytdlp_args: String::new(),
        }
    }
}

impl Config {
    pub fn load() -> Self {
        let mut cfg: Self = std::fs::read_to_string(crate::paths::config_path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        if cfg.upgrade_local_model(crate::paths::total_ram_gb()) {
            let _ = cfg.save();
        }
        cfg
    }

    /// Troca a IA local da versão anterior (Qwen 2.5) pela nova do mesmo nível, sem passar
    /// do que a memória do computador aguenta. O arquivo novo é baixado sozinho no próximo uso.
    pub fn upgrade_local_model(&mut self, ram_gb: f64) -> bool {
        if crate::models::upgraded_llm(&self.llm_local_model).is_none() {
            return false;
        }
        let order = ["fast", "balanced", "best"];
        let old_level = match self.llm_local_model.as_str() {
            "qwen2.5-1.5b-instruct-q4_k_m.gguf" => 0,
            "qwen2.5-3b-instruct-q4_k_m.gguf" => 1,
            _ => 2,
        };
        let ram_level = order
            .iter()
            .position(|t| *t == crate::models::recommended_tier(ram_gb))
            .unwrap_or(1);
        let tier = order[old_level.min(ram_level)];
        let Some(t) = crate::models::tiers().into_iter().find(|t| t.id == tier) else {
            return false;
        };
        self.llm_local_model = t.llm_file.to_string();
        if !self.tier.is_empty() {
            self.tier = tier.to_string();
        }
        true
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
            std::thread::available_parallelism()
                .map(|n| n.get() as u32)
                .unwrap_or(4)
                .clamp(1, 16)
        }
    }

    /// Opções da estrutura `id` (as salvas pela pessoa ou as padrão).
    pub fn style(&self, id: &str) -> crate::structures::StyleOpts {
        match self.styles.get(id) {
            Some(saved) => crate::structures::effective(id, saved),
            None => crate::structures::defaults(id),
        }
    }

    /// Texto da marca d'água ("@perfil"), ou None se estiver desligada.
    pub fn watermark_text(&self) -> Option<String> {
        let h: String = self
            .watermark
            .trim()
            .trim_start_matches('@')
            .chars()
            .filter(|c| !c.is_whitespace())
            .take(40)
            .collect();
        (!h.is_empty()).then(|| format!("@{h}"))
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

#[cfg(test)]
mod upgrade_tests {
    use super::*;

    #[test]
    fn troca_a_ia_antiga_pela_nova_conforme_a_memoria() {
        let mut c = Config {
            llm_local_model: "qwen2.5-3b-instruct-q4_k_m.gguf".into(),
            tier: "balanced".into(),
            ..Default::default()
        };
        assert!(c.upgrade_local_model(16.0));
        assert_eq!(c.llm_local_model, "gemma-4-E4B-it-Q4_K_M.gguf");
        let mut c = Config {
            llm_local_model: "qwen2.5-3b-instruct-q4_k_m.gguf".into(),
            tier: "balanced".into(),
            ..Default::default()
        };
        assert!(c.upgrade_local_model(8.0));
        assert_eq!(c.llm_local_model, "Qwen3-4B-Instruct-2507-Q4_K_M.gguf");
        assert_eq!(c.tier, "fast");
        assert!(!c.upgrade_local_model(8.0));
    }
}
