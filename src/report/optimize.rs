use std::path::{Path, PathBuf};
use anyhow::{Context, Result};
use chrono::NaiveDate;
use serde::Serialize;
use crate::optimize::OptReport;

pub struct OptMeta {
    pub start: NaiveDate,
    pub end: NaiveDate,
    pub fund_code: String,
}

#[derive(Serialize)]
struct RunJson<'a> {
    label: String,
    summary: &'a crate::metrics::Summary,
    daily: &'a [crate::result::DailyRecord],
}

#[derive(Serialize)]
struct Payload<'a> {
    start: String,
    end: String,
    // 仅 Top-N 进图，避免曲线过多
    runs: Vec<RunJson<'a>>,
}

fn fmt_pct(v: f64) -> String { format!("{:.2}%", v * 100.0) }
fn sign_class(v: f64) -> &'static str { if v >= 0.0 { "pos" } else { "neg" } }

/// 取 toml 值的紧凑显示（去掉字符串引号，整数/浮点原样）。
fn cell(v: Option<&toml::Value>) -> String {
    match v {
        Some(toml::Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

pub fn render_optimize_html(meta: &OptMeta, report: &OptReport) -> String {
    let top_n = report.top_n.min(report.ranked.len());
    let payload = Payload {
        start: meta.start.to_string(),
        end: meta.end.to_string(),
        runs: report.ranked.iter().take(top_n).map(|o| RunJson {
            label: o.label.clone(),
            summary: &o.outcome.summary,
            daily: &o.outcome.daily,
        }).collect(),
    };
    let data_json = serde_json::to_string(&payload)
        .expect("序列化寻优数据失败")
        .replace("</", "<\\/");
    build_html(meta, report, &data_json)
}

pub fn render_optimize(meta: &OptMeta, report: &OptReport, out_dir: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(out_dir)
        .with_context(|| format!("创建输出目录失败: {}", out_dir.display()))?;
    let html = render_optimize_html(meta, report);
    let out = out_dir.join("optimize.html");
    std::fs::write(&out, html.as_bytes())
        .with_context(|| format!("写入 {} 失败", out.display()))?;
    Ok(out)
}

fn build_html(meta: &OptMeta, report: &OptReport, data_json: &str) -> String {
    let fund_esc = crate::report::html_escape(&meta.fund_code);
    let strat_esc = crate::report::html_escape(&report.strategy);
    let metric_esc = crate::report::html_escape(&report.metric);
    let total = report.ranked.len();
    let top_n = report.top_n.min(total);

    // 表头参数列
    let param_th: String = report.param_keys.iter()
        .map(|k| format!("<th>{}</th>", crate::report::html_escape(k)))
        .collect();

    // 排序所依指标对应的列名（用于整列高亮 class）
    let metric_col = report.metric.as_str();

    let has_oos = report.split_ratio.is_some();

    let rows: String = report.ranked.iter().enumerate().map(|(i, o)| {
        let s = &o.outcome.summary;
        let t = o.params.as_table();
        let param_tds: String = report.param_keys.iter()
            .map(|k| format!("<td>{}</td>", crate::report::html_escape(&cell(t.and_then(|tt| tt.get(k))))))
            .collect();
        let rank_best = if i == 0 { " best" } else { "" };
        let hl = |col: &str| if col == metric_col { " best" } else { "" };

        // 检验段列。训练段是 argmax 出来的（必然好看），检验段才是没见过的数据 ——
        // 故这里刻意给检验段加底色高亮，让用户先看它。
        let oos_tds = match &o.oos {
            Some(oo) => {
                let os = &oo.summary;
                format!(
                    "<td class=\"oos {c1}\">{r1}</td><td class=\"oos\">{s1:.2}</td><td class=\"oos neg\">{m1}</td>",
                    c1 = sign_class(os.total_return), r1 = fmt_pct(os.total_return),
                    s1 = os.sharpe, m1 = fmt_pct(os.max_drawdown))
            }
            None => "<td class=\"oos\" colspan=\"3\">—（数据不足，无检验段）</td>".to_string(),
        };

        format!(
            "<tr>\
<td class=\"{rank_best}\">{rank}</td>\
{params}\
<td class=\"{cret}{h_ret}\">{ret}</td>\
<td class=\"{h_sharpe}\">{sharpe:.2}</td>\
<td class=\"neg{h_mdd}\">{mdd}</td>\
{oos_tds}\
<td>{tc}</td>\
</tr>\n",
            rank_best = rank_best,
            rank = i + 1,
            params = param_tds,
            cret = sign_class(s.total_return), h_ret = hl("total_return"), ret = fmt_pct(s.total_return),
            h_sharpe = hl("sharpe"), sharpe = s.sharpe,
            h_mdd = hl("max_drawdown"), mdd = fmt_pct(s.max_drawdown),
            oos_tds = oos_tds,
            tc = s.trade_count,
        )
    }).collect();

    let n_params = report.param_keys.len().max(1);
    let split_desc = match report.split_ratio {
        Some(r) => format!("训练段 前{:.0}% / 检验段 后{:.0}%", r * 100.0, (1.0 - r) * 100.0),
        None => "未切分（全部为样本内）".to_string(),
    };
    let caveat_html = crate::report::html_escape(&report.caveat).replace('\n', "<br/>");
    let oos_group_th = if has_oos {
        "<th class=\"oos\" colspan=\"3\">检验段（样本外 · 请看这里）</th>"
    } else {
        "<th class=\"oos\" colspan=\"3\">检验段（无）</th>"
    };

    format!(r#"<!DOCTYPE html>
<html lang="zh-CN">
<head>
<meta charset="UTF-8"/>
<meta name="viewport" content="width=device-width,initial-scale=1"/>
<title>参数寻优报告</title>
<style>
*,*::before,*::after{{box-sizing:border-box;margin:0;padding:0}}
body{{font-family:-apple-system,BlinkMacSystemFont,"Segoe UI",Roboto,"Helvetica Neue",Arial,sans-serif;background:#f5f6fa;color:#2c3e50;line-height:1.5}}
.container{{max-width:1200px;margin:0 auto;padding:24px 16px}}
header{{margin-bottom:24px}}
header h1{{font-size:1.8rem;font-weight:700;color:#1a252f}}
header .subtitle{{color:#7f8c8d;margin-top:4px;font-size:.95rem}}
.card{{background:#fff;border:1px solid #e0e4ea;border-radius:10px;padding:20px;margin-bottom:20px;box-shadow:0 1px 4px rgba(0,0,0,.06)}}
.card h2{{font-size:1rem;font-weight:600;color:#34495e;margin-bottom:14px;border-bottom:1px solid #eaecef;padding-bottom:8px}}
.pos{{color:#c0392b}}
.neg{{color:#27ae60}}
.chart{{width:100%;height:380px}}
table{{width:100%;border-collapse:collapse;font-size:.88rem}}
th{{background:#f0f2f5;text-align:left;padding:8px 10px;font-weight:600;color:#5a6a7a;position:sticky;top:0}}
td{{padding:7px 10px;border-bottom:1px solid #f0f2f5}}
tr:last-child td{{border-bottom:none}}
.best{{font-weight:700;background:#fffbe6}}
.scrollable{{overflow-x:auto}}
.oos{{background:#eef7f0}}
.caveat{{background:#fffbf0;border-left:4px solid #b8860b;color:#5a4a1a}}
.caveat h2{{color:#8a6d1a;border-bottom-color:#e8d9a8}}
</style>
</head>
<body>
<div class="container">
<header>
  <h1>参数寻优报告</h1>
  <div class="subtitle">基金 {fund} &nbsp;|&nbsp; 策略 {strat} &nbsp;|&nbsp; 区间 {start} ~ {end} &nbsp;|&nbsp; 排序指标 {metric} &nbsp;|&nbsp; 共 {total} 组合（图示 Top {top_n}）&nbsp;|&nbsp; {split_desc}</div>
</header>

<div class="card caveat">
  <h2>⚠ 关于「最优参数」，先读这段</h2>
  <div>{caveat_html}</div>
</div>

<div class="card">
  <h2>组合排名</h2>
  <div class="scrollable">
  <table>
    <thead>
      <tr><th rowspan="2">排名</th><th colspan="{n_params}">参数</th><th colspan="3">训练段（选参数用 · 必然好看）</th>{oos_group_th}<th rowspan="2">交易次数</th></tr>
      <tr>{param_th}<th>总收益</th><th>夏普</th><th>最大回撤</th><th class="oos">总收益</th><th class="oos">夏普</th><th class="oos">最大回撤</th></tr>
    </thead>
    <tbody>
{rows}    </tbody>
  </table>
  </div>
</div>

<div class="card">
  <h2>Top {top_n} 收益率对比</h2>
  <div id="chart-return" class="chart"></div>
</div>

</div><!-- /container -->

<script>const DATA = {data_json};</script>
<script src="https://cdn.jsdelivr.net/npm/echarts@5/dist/echarts.min.js"></script>
<script>
(function(){{
  if (typeof echarts === 'undefined') {{
    document.querySelectorAll('.chart').forEach(function(e) {{
      e.style.display='flex';e.style.alignItems='center';e.style.justifyContent='center';
      e.style.color='#888';e.style.fontSize='14px';
      e.innerHTML='图表库加载失败（需联网加载 ECharts）';
    }});
    return;
  }}
  var COLORS = ['#c0392b','#2980b9','#27ae60','#8e44ad','#e67e22','#16a085'];
  var retSeries = DATA.runs.map(function(run, idx) {{
    var cum = 0;
    var data = run.daily.map(function(r) {{
      cum += r.contribution;
      var ret = cum > 0 ? (r.equity / cum - 1) * 100 : 0;
      return [r.date, +ret.toFixed(4)];
    }});
    return {{ name: run.label, type:'line', data:data, smooth:true, symbol:'none',
      lineStyle:{{color:COLORS[idx % COLORS.length]}}, itemStyle:{{color:COLORS[idx % COLORS.length]}} }};
  }});
  var _dateSet = new Set();
  DATA.runs.forEach(function(run) {{ run.daily.forEach(function(r) {{ _dateSet.add(r.date); }}); }});
  var allDates = Array.from(_dateSet).sort();
  var c1 = echarts.init(document.getElementById('chart-return'));
  c1.setOption({{
    tooltip: {{ trigger:'axis', valueFormatter:function(v){{ return (typeof v==='number'?v.toFixed(2)+'%':v); }} }},
    legend: {{ data: DATA.runs.map(function(r){{ return r.label; }}), bottom:36 }},
    grid: {{ left:60, right:20, top:40, bottom:80 }},
    xAxis: {{ type:'category', data:allDates, boundaryGap:false }},
    yAxis: {{ type:'value', name:'%', axisLabel:{{ formatter:function(v){{return v+'%';}} }} }},
    dataZoom: [{{ type:'slider', xAxisIndex:0, bottom:8, height:20 }}, {{ type:'inside', xAxisIndex:0 }}],
    series: retSeries
  }});
  window.addEventListener('resize', function() {{ c1.resize(); }});
}})();
</script>
</body>
</html>
"#,
        fund = fund_esc,
        strat = strat_esc,
        metric = metric_esc,
        start = meta.start,
        end = meta.end,
        total = total,
        top_n = top_n,
        param_th = param_th,
        rows = rows,
        data_json = data_json,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use crate::metrics::Summary;
    use crate::result::DailyRecord;
    use crate::runner::RunOutcome;
    use crate::optimize::{OptOutcome, OptReport};

    fn d(y: i32, m: u32, day: u32) -> NaiveDate { NaiveDate::from_ymd_opt(y, m, day).unwrap() }

    fn daily() -> Vec<DailyRecord> {
        vec![
            DailyRecord { date: d(2024, 1, 1), nav: 1.0, adj_nav: 1.0, equity: 1000.0, contribution: 1000.0, shares: 1000.0, cash: 0.0 },
            DailyRecord { date: d(2024, 2, 15), nav: 2.0, adj_nav: 2.0, equity: 2000.0, contribution: 0.0, shares: 1000.0, cash: 0.0 },
        ]
    }

    fn run(label: &str, total_return: f64) -> RunOutcome {
        RunOutcome {
            name: label.to_string(),
            fund_code: "161725".to_string(),
            summary: Summary { total_contributed: 1000.0, final_equity: 2000.0, total_return, annualized: 0.4, max_drawdown: 0.1, sharpe: 1.2, trade_count: 1 },
            daily: daily(),
        }
    }

    /// 训练段 total_return 传 `total_return`，检验段固定给一个明显更差的值 ——
    /// 报告必须把这个落差呈现出来。
    fn outcome(label: &str, total_return: f64, ma: i64) -> OptOutcome {
        let mut params = toml::Table::new();
        params.insert("ma_window".into(), toml::Value::Integer(ma));
        OptOutcome {
            params: toml::Value::Table(params),
            label: label.to_string(),
            outcome: run(label, total_return),
            oos: Some(run(label, total_return * 0.2)),   // 样本外大幅衰减
        }
    }

    #[test]
    fn render_optimize_html_returns_markup() {
        let report = OptReport {
            strategy: "smart_dca".to_string(), metric: "total_return".to_string(), top_n: 5,
            ranked: vec![outcome("ma_window=250", 1.0, 250)],
            param_keys: vec!["ma_window".to_string()],
            split_ratio: Some(0.70), combos: 1,
            caveat: "参数在训练段上选出，请只看检验段的数字。".to_string(),
        };
        let meta = OptMeta { start: d(2024,1,1), end: d(2024,2,15), fund_code: "161725".to_string() };
        let html = render_optimize_html(&meta, &report);
        assert!(html.contains("参数寻优"));
        assert!(html.contains("ma_window"));
        assert!(html.contains("const DATA"));
    }

    #[test]
    fn renders_optimize_report() {
        let report = OptReport {
            strategy: "smart_dca".to_string(),
            metric: "total_return".to_string(),
            top_n: 5,
            ranked: vec![outcome("ma_window=250", 1.0, 250), outcome("ma_window=120", 0.5, 120)],
            param_keys: vec!["ma_window".to_string()],
            split_ratio: Some(0.70), combos: 2,
            caveat: "参数在训练段上选出，请只看检验段的数字。".to_string(),
        };
        let meta = OptMeta { start: d(2024, 1, 1), end: d(2024, 2, 15), fund_code: "161725".to_string() };
        let tmp = std::env::temp_dir().join("xlh_optimize_test");
        let path = render_optimize(&meta, &report, &tmp).unwrap();

        assert!(path.exists(), "optimize.html should exist");
        let html = std::fs::read_to_string(&path).unwrap();
        assert!(html.contains("参数寻优"), "标题");
        assert!(html.contains("排名"), "排名表头");
        assert!(html.contains("ma_window"), "参数列名");
        assert!(html.contains("ma_window=250"), "组合标签");
        assert!(html.contains("const DATA"), "内嵌数据");
        assert!(html.contains("echarts"), "图表库");
        assert!(html.contains("<table"), "表格");

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
