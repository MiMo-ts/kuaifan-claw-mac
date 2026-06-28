// 数据结构模块
use serde::{Deserialize, Serialize};

// 环境检测状态
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum EnvStatus {
    Success,
    Warning,
    Error,
    Checking,
}

// 环境检测项
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvItem {
    pub name: String,
    pub version: Option<String>,
    pub status: EnvStatus,
    pub message: String,
    pub required: bool,
}

// 环境检测结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvCheckResult {
    pub success: bool,
    pub items: Vec<EnvItem>,
    pub recommendations: Vec<String>,
}

// 一键修复环境（安装脚本执行记录）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvAutoFixResult {
    pub ok: bool,
    pub messages: Vec<String>,
}

// 安装进度
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallProgress {
    pub step: String,
    pub progress: f32,
    pub message: String,
    pub status: String,
}

/// 向导「安装 OpenClaw-CN」步骤：用于检测是否可跳过整步安装。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenClawCnStatus {
    /// 存在 `dist/entry.js`（网关/CLI 入口）
    pub core_ready: bool,
    /// `node_modules` 已含核心依赖（可启动网关）
    pub deps_ready: bool,
    /// 核心 + 依赖均就绪，向导可直接「下一步」
    pub fully_ready: bool,
    pub version: Option<String>,
    pub openclaw_dir: String,
}

// 插件信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginInfo {
    pub id: String,
    pub name: String,
    pub icon: String,
    pub description: String,
    pub installed: bool,
    pub version: Option<String>,
    pub enabled: bool,
    /// 运行时 npm 依赖是否就绪（channel_plugin_runtime_ready），就绪时网关才能正常加载
    pub deps_ready: bool,
}

// 模型供应商
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelProvider {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub api_key_configured: bool,
    pub free_models_count: usize,
    pub total_models_count: usize,
}

// 模型配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelConfig {
    pub provider: String,
    pub model_name: String,
    pub api_key: Option<String>,
    pub api_base: Option<String>,
    pub temperature: f32,
    pub max_tokens: usize,
}

// 机器人模板
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RobotTemplate {
    pub id: String,
    pub category: String,
    pub subcategory: String,
    pub name: String,
    pub description: String,
    pub icon: String,
    pub color: String,
    pub system_prompt: String,
    pub default_skills: Vec<String>,
    pub default_mcp: Vec<String>,
    pub tags: Vec<String>,
}

/// 向导展示的 MCP 推荐项（需在 OpenClaw 侧自行接入，管理器不自动安装 MCP 进程）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpRecommendation {
    pub id: String,
    pub name: String,
    pub description: String,
    pub setup_note: String,
    /// 常见 MCP 实现可能依赖云端嵌入/搜索等 API Key（仅提示用）
    #[serde(default)]
    pub requires_api_key: bool,
}

// Skill 信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillInfo {
    pub id: String,
    pub name: String,
    pub description: String,
    pub license: String,
    pub stars: usize,
    pub free: bool,
    pub downloaded: bool,
    pub notice: Option<String>,
}

// 机器人
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Robot {
    pub id: String,
    pub name: String,
    pub category: String,
    pub description: String,
    pub icon: String,
    pub color: String,
    /// 该机器人模板对应的专属技能 ID 列表（来自 builtin_robot_templates 或用户自定义）
    pub skills: Vec<String>,
    pub created_at: String,
}

// 实例
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Instance {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    /// 绑定的机器人 ID，不选机器人时为 None（使用通用人设 + openclaw skills）
    #[serde(default)]
    pub robot_id: Option<String>,
    pub channel_type: String,
    pub channel_config: serde_json::Value,
    pub model: Option<ModelConfig>,
    pub max_history: usize,
    pub response_mode: String,
    pub message_count: usize,
    pub created_at: String,
    pub updated_at: String,
}

// 网关状态
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GatewayStatus {
    pub running: bool,
    pub version: Option<String>,
    pub port: u16,
    pub uptime_seconds: u64,
    pub memory_mb: f64,
    pub instances_running: usize,
}

// 备份信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupInfo {
    pub id: String,
    pub filename: String,
    pub created_at: String,
    pub size_bytes: u64,
    pub description: Option<String>,
}

// 系统信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemInfo {
    pub os: String,
    pub arch: String,
    pub cpu_count: usize,
    pub total_memory_mb: u64,
    pub available_memory_mb: u64,
    pub hostname: String,
}

// 日志条目
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    pub timestamp: String,
    pub level: String,
    pub message: String,
    pub target: Option<String>,
}

/// 设置页「运行日志」：网关 stdout/stderr 与管理端 app.log 尾部
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeLogsTail {
    pub gateway: String,
    pub manager: String,
}

// ═══════════════════════════════════════════════════════════════
// 全模型多供应商监控数据结构
// ═══════════════════════════════════════════════════════════════

/// 用量记录扩展（支持详细指标）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetailedUsageRecord {
    pub ts: String,
    pub provider: String,
    pub model: String,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
    pub source: String,
    #[serde(default)]
    pub response_time_ms: Option<u64>,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub error_message: Option<String>,
}

/// 基础用量记录（用于向后兼容）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenUsageRecord {
    pub ts: String,
    pub provider: String,
    pub model: String,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
    pub source: String,
}

impl From<TokenUsageRecord> for DetailedUsageRecord {
    fn from(r: TokenUsageRecord) -> Self {
        Self {
            ts: r.ts,
            provider: r.provider,
            model: r.model,
            prompt_tokens: r.prompt_tokens,
            completion_tokens: r.completion_tokens,
            total_tokens: r.total_tokens,
            source: r.source,
            response_time_ms: None,
            status: "success".to_string(),
            error_message: None,
        }
    }
}

/// 模型用量汇总（按模型分组）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelUsageStats {
    pub provider: String,
    pub model: String,
    pub request_count: u64,
    pub total_prompt_tokens: u64,
    pub total_completion_tokens: u64,
    pub total_tokens: u64,
    pub total_cost: f64,
    pub avg_response_time_ms: Option<f64>,
    pub error_count: u64,
    pub success_rate: f64,
}

/// 供应商用量汇总（按供应商分组）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderUsageStats {
    pub provider: String,
    pub request_count: u64,
    pub total_tokens: u64,
    pub total_cost: f64,
    pub model_count: usize,
    pub top_model: Option<String>,
}

/// 供应商定价信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderPricing {
    pub provider: String,
    pub input_cost_per_mtok: f64,
    pub output_cost_per_mtok: f64,
    pub cache_read_cost_per_mtok: f64,
    pub cache_write_cost_per_mtok: f64,
    pub currency: String,
    pub free_tier_tokens: Option<u64>,
}

impl ProviderPricing {
    pub fn calculate_cost(
        &self,
        prompt_tokens: u64,
        completion_tokens: u64,
        cache_read: u64,
        cache_write: u64,
    ) -> f64 {
        let input_cost = (prompt_tokens as f64) * self.input_cost_per_mtok / 1_000_000.0;
        let output_cost = (completion_tokens as f64) * self.output_cost_per_mtok / 1_000_000.0;
        let cache_read_cost = (cache_read as f64) * self.cache_read_cost_per_mtok / 1_000_000.0;
        let cache_write_cost = (cache_write as f64) * self.cache_write_cost_per_mtok / 1_000_000.0;
        input_cost + output_cost + cache_read_cost + cache_write_cost
    }
}

/// 成本预算配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostBudget {
    pub id: String,
    pub name: String,
    pub budget_type: BudgetType,
    pub limit_amount: f64,
    pub alert_threshold: f64,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub enabled: bool,
    pub current_spend: f64,
    pub reset_period: Option<String>,
    pub last_reset: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BudgetType {
    Daily,
    Weekly,
    Monthly,
    Total,
}

/// 成本告警记录
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostAlert {
    pub id: String,
    pub budget_id: String,
    pub alert_type: AlertType,
    pub threshold: f64,
    pub current_spend: f64,
    pub message: String,
    pub triggered_at: String,
    pub acknowledged: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AlertType {
    Threshold50,
    Threshold75,
    Threshold90,
    Threshold100,
}

/// 实时监控指标
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RealTimeMetrics {
    pub timestamp: String,
    pub provider: String,
    pub model: String,
    pub requests_total: u64,
    pub requests_success: u64,
    pub requests_error: u64,
    pub tokens_total: u64,
    pub cost_total: f64,
    pub avg_response_time_ms: f64,
    pub rpm: f64,
    pub tpm: u64,
}
