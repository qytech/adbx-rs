use ratatui::{
    style::{Color, Modifier, Style},
    text::Span,
};
use regex::{Regex, RegexBuilder};
use std::collections::BTreeMap;
use std::sync::OnceLock;

pub struct CustomRule {
    pub regex: Regex,
    pub style: Style,
}

struct BuiltinRule {
    regex: Regex,
    style: Style,
}

static BUILTIN_RULES: OnceLock<Vec<BuiltinRule>> = OnceLock::new();

fn builtin_rules() -> &'static [BuiltinRule] {
    BUILTIN_RULES.get_or_init(|| {
        vec![
            // 1. 崩溃 / 异常 / 错误关键词（亮红加粗）
            BuiltinRule {
                regex: Regex::new(
                    r"\b(?:FATAL|EXCEPTION|ANR|CRASH|ERROR|FAILED|NullPointerException|OutOfMemoryError|IllegalStateException|RuntimeException)\b",
                )
                .unwrap(),
                style: Style::default().fg(Color::LightRed).add_modifier(Modifier::BOLD),
            },
            // 2. URL 链接（青色下划线，类似 VS Code）
            BuiltinRule {
                regex: Regex::new(r"https?://[^\s,;\)\]]+").unwrap(),
                style: Style::default()
                    .fg(Color::LightCyan)
                    .add_modifier(Modifier::UNDERLINED),
            },
            // 3. HTTP 方法与状态码
            BuiltinRule {
                regex: Regex::new(r"\b(?:GET|POST|PUT|DELETE|PATCH|HEAD)\b").unwrap(),
                style: Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            },
            BuiltinRule {
                regex: Regex::new(r"\b(?:5\d{2})\b").unwrap(),
                style: Style::default().fg(Color::LightRed).add_modifier(Modifier::BOLD),
            },
            BuiltinRule {
                regex: Regex::new(r"\b(?:4\d{2})\b").unwrap(),
                style: Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            },
            // 4. Java/Kotlin 堆栈行号 `(MainActivity.kt:128)`（亮黄）
            BuiltinRule {
                regex: Regex::new(r"\([A-Za-z0-9_]+\.(?:java|kt|cpp|cc|rs):\d+\)").unwrap(),
                style: Style::default().fg(Color::LightYellow),
            },
            // 5. 双引号字符串（柔绿，类似 VS Code 字符串配色）
            BuiltinRule {
                regex: Regex::new(r#""[^"\n]{1,120}""#).unwrap(),
                style: Style::default().fg(Color::LightGreen),
            },
            // 6. IP 地址 / 耗时与容量数字（如 128ms, 45.2MB, 99%, 192.168.1.2:8080）
            BuiltinRule {
                regex: Regex::new(
                    r"\b(?:\d{1,3}(?:\.\d{1,3}){3}(?::\d+)?|\d+(?:\.\d+)?(?:ms|us|ns|s|MB|KB|GB|B|%))\b",
                )
                .unwrap(),
                style: Style::default().fg(Color::LightMagenta),
            },
            // 7. Key=Value 键名（浅蓝）
            BuiltinRule {
                regex: Regex::new(r"\b[a-zA-Z_][a-zA-Z0-9_]*=").unwrap(),
                style: Style::default().fg(Color::LightBlue),
            },
            // 8. 布尔与成功词
            BuiltinRule {
                regex: Regex::new(r"\b(?:true|SUCCESS|OK)\b").unwrap(),
                style: Style::default().fg(Color::Green),
            },
            BuiltinRule {
                regex: Regex::new(r"\b(?:false|null|nil|NONE)\b").unwrap(),
                style: Style::default().fg(Color::DarkGray),
            },
        ]
    })
}

/// 将 devices.toml 中的 `[log_highlights]` 编译为正则规则
pub fn compile_custom_rules(raw_map: &BTreeMap<String, String>) -> Vec<CustomRule> {
    raw_map
        .iter()
        .filter_map(|(pattern, color_name)| {
            let re = RegexBuilder::new(pattern).case_insensitive(true).build().ok()?;
            Some(CustomRule {
                regex: re,
                style: parse_color_style(color_name),
            })
        })
        .collect()
}

pub fn parse_color_style(name: &str) -> Style {
    match name.trim().to_ascii_lowercase().as_str() {
        "red" => Style::default().fg(Color::LightRed).add_modifier(Modifier::BOLD),
        "green" => Style::default().fg(Color::LightGreen).add_modifier(Modifier::BOLD),
        "yellow" => Style::default().fg(Color::LightYellow).add_modifier(Modifier::BOLD),
        "blue" => Style::default().fg(Color::LightBlue).add_modifier(Modifier::BOLD),
        "magenta" | "purple" => Style::default().fg(Color::LightMagenta).add_modifier(Modifier::BOLD),
        "cyan" => Style::default().fg(Color::LightCyan).add_modifier(Modifier::BOLD),
        "red_bg" => Style::default().fg(Color::White).bg(Color::Red).add_modifier(Modifier::BOLD),
        "yellow_bg" => Style::default().fg(Color::Black).bg(Color::Yellow).add_modifier(Modifier::BOLD),
        "green_bg" => Style::default().fg(Color::Black).bg(Color::Green).add_modifier(Modifier::BOLD),
        "cyan_bg" => Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD),
        _ => Style::default().fg(Color::LightYellow),
    }
}

/// 根据 Tag 名称生成稳定且易区分的颜色（类似 Android Studio / pidcat）
pub fn tag_color(tag: &str) -> Color {
    const PALETTE: &[Color] = &[
        Color::Cyan,
        Color::LightBlue,
        Color::LightGreen,
        Color::LightMagenta,
        Color::LightYellow,
        Color::LightCyan,
    ];
    let hash = tag
        .bytes()
        .fold(0usize, |acc, b| acc.wrapping_mul(31).wrapping_add(b as usize));
    PALETTE[hash % PALETTE.len()]
}

/// 将一条日志消息按优先级拆分为彩色 Span：
/// 优先级：搜索匹配(/regex) > 用户自定义规则([log_highlights]) > 内置 VS Code 语义高亮
pub fn highlight_message<'a>(
    msg: &'a str,
    search_re: Option<&Regex>,
    custom_rules: &[CustomRule],
    enabled: bool,
) -> Vec<Span<'a>> {
    if !enabled || msg.is_empty() {
        return vec![Span::raw(msg)];
    }

    let len = msg.len();
    // 为每个字节槽记录最先命中的高优先级样式（UTF-8 安全：正则匹配边界必为字符边界）
    let mut styles: Vec<Option<Style>> = vec![None; len];

    // 1. 最高优先级：当前 `/` 搜索命中（VS Code 亮黄底黑字）
    if let Some(re) = search_re {
        let match_style = Style::default()
            .fg(Color::Black)
            .bg(Color::Yellow)
            .add_modifier(Modifier::BOLD);
        apply_regex_style(msg, re, match_style, &mut styles);
    }

    // 2. 次优先级：用户在 devices.toml 自定义的 [log_highlights]
    for rule in custom_rules {
        apply_regex_style(msg, &rule.regex, rule.style, &mut styles);
    }

    // 3. 基础语义高亮（URL / Exception / JSON 字符串 / 耗时 / Key= / 堆栈行号）
    for rule in builtin_rules() {
        apply_regex_style(msg, &rule.regex, rule.style, &mut styles);
    }

    // 合并相邻同样式字节区间为最少数量的 Span
    let mut spans = Vec::new();
    let mut start = 0usize;
    while start < len {
        let cur_style = styles[start];
        let mut end = start + 1;
        while end < len && styles[end] == cur_style {
            end += 1;
        }
        // 确保切片落在 UTF-8 字符边界上
        while end < len && !msg.is_char_boundary(end) {
            end += 1;
        }
        let slice = &msg[start..end];
        match cur_style {
            Some(st) => spans.push(Span::styled(slice, st)),
            None => spans.push(Span::raw(slice)),
        }
        start = end;
    }

    spans
}

fn apply_regex_style(msg: &str, re: &Regex, style: Style, styles: &mut [Option<Style>]) {
    for m in re.find_iter(msg) {
        let range = m.start()..m.end();
        // 仅在尚未被更高优先级规则涂色时着色
        if styles[range.clone()].iter().all(|s| s.is_none()) {
            for slot in &mut styles[range] {
                *slot = Some(style);
            }
        }
    }
}

/// 将 ratatui Color 转换为标准 ANSI 前景色转义序列
pub fn color_to_ansi_fg(c: Color) -> &'static str {
    match c {
        Color::Black => "\x1b[30m",
        Color::Red => "\x1b[31m",
        Color::Green => "\x1b[32m",
        Color::Yellow => "\x1b[33m",
        Color::Blue => "\x1b[34m",
        Color::Magenta => "\x1b[35m",
        Color::Cyan => "\x1b[36m",
        Color::Gray => "\x1b[37m",
        Color::DarkGray => "\x1b[90m",
        Color::LightRed => "\x1b[91m",
        Color::LightGreen => "\x1b[92m",
        Color::LightYellow => "\x1b[93m",
        Color::LightBlue => "\x1b[94m",
        Color::LightMagenta => "\x1b[95m",
        Color::LightCyan => "\x1b[96m",
        Color::White => "\x1b[97m",
        _ => "",
    }
}

fn color_to_sgr_fg(c: Color) -> Option<&'static str> {
    match c {
        Color::Black => Some("30"),
        Color::Red => Some("31"),
        Color::Green => Some("32"),
        Color::Yellow => Some("33"),
        Color::Blue => Some("34"),
        Color::Magenta => Some("35"),
        Color::Cyan => Some("36"),
        Color::Gray => Some("37"),
        Color::DarkGray => Some("90"),
        Color::LightRed => Some("91"),
        Color::LightGreen => Some("92"),
        Color::LightYellow => Some("93"),
        Color::LightBlue => Some("94"),
        Color::LightMagenta => Some("95"),
        Color::LightCyan => Some("96"),
        Color::White => Some("97"),
        _ => None,
    }
}

fn color_to_sgr_bg(c: Color) -> Option<&'static str> {
    match c {
        Color::Black => Some("40"),
        Color::Red => Some("41"),
        Color::Green => Some("42"),
        Color::Yellow => Some("43"),
        Color::Blue => Some("44"),
        Color::Magenta => Some("45"),
        Color::Cyan => Some("46"),
        Color::Gray => Some("47"),
        Color::DarkGray => Some("100"),
        Color::LightRed => Some("101"),
        Color::LightGreen => Some("102"),
        Color::LightYellow => Some("103"),
        Color::LightBlue => Some("104"),
        Color::LightMagenta => Some("105"),
        Color::LightCyan => Some("106"),
        Color::White => Some("107"),
        _ => None,
    }
}

fn style_to_ansi(style: Style) -> String {
    let mut codes: Vec<&'static str> = Vec::new();
    if style.add_modifier.contains(Modifier::BOLD) {
        codes.push("1");
    }
    if style.add_modifier.contains(Modifier::UNDERLINED) {
        codes.push("4");
    }
    if let Some(fg) = style.fg.and_then(color_to_sgr_fg) {
        codes.push(fg);
    }
    if let Some(bg) = style.bg.and_then(color_to_sgr_bg) {
        codes.push(bg);
    }
    if codes.is_empty() {
        String::new()
    } else {
        format!("\x1b[{}m", codes.join(";"))
    }
}

/// 将单条日志消息渲染为带 VS Code 语义与自定义规则高亮的 ANSI 字符串
pub fn render_ansi_message(msg: &str, custom_rules: &[CustomRule], default_ansi: &str) -> String {
    if msg.is_empty() {
        return String::new();
    }
    let spans = highlight_message(msg, None, custom_rules, true);
    let mut out = String::with_capacity(msg.len() + 32);
    for span in spans {
        if span.style == Style::default() {
            if default_ansi.is_empty() {
                out.push_str(span.content.as_ref());
            } else {
                out.push_str(default_ansi);
                out.push_str(span.content.as_ref());
                out.push_str("\x1b[0m");
            }
        } else {
            let ansi = style_to_ansi(span.style);
            if ansi.is_empty() {
                out.push_str(span.content.as_ref());
            } else {
                out.push_str(&ansi);
                out.push_str(span.content.as_ref());
                out.push_str("\x1b[0m");
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highlights_utf8_message_with_search_and_semantics() {
        let msg = r#"请求失败 POST https://api.example.com/pay status=500 耗时 245ms "超时""#;
        let search = Regex::new("pay").unwrap();
        let spans = highlight_message(msg, Some(&search), &[], true);
        let joined: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(joined, msg);
        assert!(spans.len() > 3);
    }

    #[test]
    fn custom_rules_tag_color_and_ansi_render_work() {
        let mut map = BTreeMap::new();
        map.insert("ORDER_PAY".to_string(), "red_bg".to_string());
        let rules = compile_custom_rules(&map);
        assert_eq!(rules.len(), 1);

        assert_eq!(tag_color("OkHttp"), tag_color("OkHttp"));

        let ansi = render_ansi_message("ORDER_PAY https://a.b 12ms ok", &rules, "\x1b[33m");
        assert!(ansi.contains("ORDER_PAY"));
        assert!(ansi.contains("\x1b[1;97;41m"));
        assert!(ansi.contains("\x1b[33m"));
        assert!(ansi.contains("\x1b[0m"));
    }
}
