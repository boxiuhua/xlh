use std::path::{Path, PathBuf};
use anyhow::{Context, Result};
use chrono::NaiveDate;
use serde::Serialize;
use crate::runner::RunOutcome;

pub struct CompareMeta {
    pub start: NaiveDate,
    pub end: NaiveDate,
}

#[derive(Serialize)]
struct RunJson<'a> {
    name: String,
    fund_code: String,
    summary: &'a crate::metrics::Summary,
    daily: &'a [crate::result::DailyRecord],
}

#[derive(Serialize)]
struct Payload<'a> {
    start: String,
    end: String,
    runs: Vec<RunJson<'a>>,
}

pub fn render_compare_html(meta: &CompareMeta, runs: &[RunOutcome]) -> String {
    let payload = Payload {
        start: meta.start.to_string(),
        end: meta.end.to_string(),
        runs: runs.iter().map(|r| RunJson {
            name: r.name.clone(),
            fund_code: r.fund_code.clone(),
            summary: &r.summary,
            daily: &r.daily,
        }).collect(),
    };
    let data_json = serde_json::to_string(&payload)
        .expect("序列化对比数据失败")
        .replace("</", "<\\/");
    build_html(meta, runs, &data_json)
}

pub fn render_compare(meta: &CompareMeta, runs: &[RunOutcome], out_dir: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(out_dir)
        .with_context(|| format!("创建输出目录失败: {}", out_dir.display()))?;
    let html = render_compare_html(meta, runs);
    let out = out_dir.join("compare.html");
    std::fs::write(&out, html.as_bytes())
        .with_context(|| format!("写入 {} 失败", out.display()))?;
    Ok(out)
}

fn fmt_pct(v: f64) -> String {
    format!("{:.2}%", v * 100.0)
}

fn sign_class(v: f64) -> &'static str {
    if v >= 0.0 { "pos" } else { "neg" }
}

fn build_html(meta: &CompareMeta, runs: &[RunOutcome], data_json: &str) -> String {
    let n = runs.len();

    // Compute best per column (indices into runs)
    let best_ret = runs.iter().enumerate()
        .max_by(|a, b| a.1.summary.total_return.partial_cmp(&b.1.summary.total_return).unwrap())
        .map(|(i, _)| i);
    let best_ann = runs.iter().enumerate()
        .max_by(|a, b| a.1.summary.annualized.partial_cmp(&b.1.summary.annualized).unwrap())
        .map(|(i, _)| i);
    let best_mdd = runs.iter().enumerate()
        .min_by(|a, b| a.1.summary.max_drawdown.partial_cmp(&b.1.summary.max_drawdown).unwrap())
        .map(|(i, _)| i);
    let best_sharpe = runs.iter().enumerate()
        .max_by(|a, b| a.1.summary.sharpe.partial_cmp(&b.1.summary.sharpe).unwrap())
        .map(|(i, _)| i);
    let best_equity = runs.iter().enumerate()
        .max_by(|a, b| a.1.summary.final_equity.partial_cmp(&b.1.summary.final_equity).unwrap())
        .map(|(i, _)| i);

    let table_rows: String = runs.iter().enumerate().map(|(i, r)| {
        let s = &r.summary;
        let name_esc = super::html_escape(&r.name);
        let fund_esc = super::html_escape(&r.fund_code);
        let c_ret = sign_class(s.total_return);
        let c_ann = sign_class(s.annualized);
        let best_ret_cls = if best_ret == Some(i) { " best" } else { "" };
        let best_ann_cls = if best_ann == Some(i) { " best" } else { "" };
        let best_mdd_cls = if best_mdd == Some(i) { " best" } else { "" };
        let best_sharpe_cls = if best_sharpe == Some(i) { " best" } else { "" };
        let best_equity_cls = if best_equity == Some(i) { " best" } else { "" };

        format!(
            "<tr>\
<td>{name}</td>\
<td>{fund}</td>\
<td class=\"{cr}{br}\">{ret}</td>\
<td class=\"{ca}{ba}\">{ann}</td>\
<td class=\"neg{bm}\">{mdd}</td>\
<td class=\"{bs}\">{sharpe:.2}</td>\
<td class=\"{be}\">{eq:.2}</td>\
<td>{contrib:.2}</td>\
<td>{tc}</td>\
</tr>\n",
            name = name_esc,
            fund = fund_esc,
            cr = c_ret, br = best_ret_cls,
            ret = fmt_pct(s.total_return),
            ca = c_ann, ba = best_ann_cls,
            ann = fmt_pct(s.annualized),
            bm = best_mdd_cls,
            mdd = fmt_pct(s.max_drawdown),
            bs = best_sharpe_cls,
            sharpe = s.sharpe,
            be = best_equity_cls,
            eq = s.final_equity,
            contrib = s.total_contributed,
            tc = s.trade_count,
        )
    }).collect();

    format!(r#"<!DOCTYPE html>
<html lang="zh-CN">
<head>
<meta charset="UTF-8"/>
<meta name="viewport" content="width=device-width,initial-scale=1"/>
<title>策略对比报告</title>
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
</style>
</head>
<body>
<div class="container">
<header>
  <h1>策略对比报告</h1>
  <div class="subtitle">回测区间：{start} ~ {end} &nbsp;|&nbsp; 共 {n} 个策略</div>
</header>

<div class="card">
  <h2>对比指标</h2>
  <div class="scrollable">
  <table>
    <thead><tr><th>策略</th><th>基金</th><th>总收益</th><th>年化</th><th>最大回撤</th><th>夏普</th><th>期末市值</th><th>累计投入</th><th>交易次数</th></tr></thead>
    <tbody>
{table_rows}    </tbody>
  </table>
  </div>
</div>

<div class="card">
  <h2>收益率对比</h2>
  <div id="chart-return" class="chart"></div>
</div>

<div class="card">
  <h2>回撤对比</h2>
  <div id="chart-drawdown" class="chart"></div>
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
  var commonDataZoom = [
    {{ type:'slider', xAxisIndex:0, bottom:8, height:20 }},
    {{ type:'inside', xAxisIndex:0 }}
  ];

  // ── Chart 1: cumulative return % ─────────────────────────────────────────
  var retSeries = DATA.runs.map(function(run, idx) {{
    var cum = 0;
    var data = run.daily.map(function(r) {{
      cum += r.contribution;
      var ret = cum > 0 ? (r.equity / cum - 1) * 100 : 0;
      return [r.date, +ret.toFixed(4)];
    }});
    return {{
      name: run.name,
      type: 'line',
      data: data,
      smooth: true,
      symbol: 'none',
      lineStyle: {{ color: COLORS[idx % COLORS.length] }},
      itemStyle: {{ color: COLORS[idx % COLORS.length] }}
    }};
  }});

  var _dateSet = new Set();
  DATA.runs.forEach(function(run) {{
    run.daily.forEach(function(r) {{ _dateSet.add(r.date); }});
  }});
  var allDates = Array.from(_dateSet).sort();

  var c1 = echarts.init(document.getElementById('chart-return'));
  c1.setOption({{
    tooltip: {{ trigger:'axis', valueFormatter: function(v){{ return (typeof v==='number'?v.toFixed(2)+'%':v); }} }},
    legend: {{ data: DATA.runs.map(function(r){{ return r.name; }}), bottom: 36 }},
    grid: {{ left:60, right:20, top:40, bottom:80 }},
    xAxis: {{ type:'category', data: allDates, boundaryGap:false }},
    yAxis: {{ type:'value', name:'%', axisLabel:{{ formatter:function(v){{return v+'%';}} }} }},
    dataZoom: commonDataZoom,
    series: retSeries
  }});

  // ── Chart 2: drawdown % ───────────────────────────────────────────────────
  var ddSeries = DATA.runs.map(function(run, idx) {{
    var peak = -Infinity;
    var data = run.daily.map(function(r) {{
      if (r.equity > peak) peak = r.equity;
      var dd = peak > 0 ? (r.equity / peak - 1) * 100 : 0;
      return [r.date, +dd.toFixed(4)];
    }});
    return {{
      name: run.name,
      type: 'line',
      data: data,
      smooth: true,
      symbol: 'none',
      lineStyle: {{ color: COLORS[idx % COLORS.length] }},
      itemStyle: {{ color: COLORS[idx % COLORS.length] }},
      areaStyle: {{ opacity: 0.1 }}
    }};
  }});

  var c2 = echarts.init(document.getElementById('chart-drawdown'));
  c2.setOption({{
    tooltip: {{ trigger:'axis', valueFormatter: function(v){{ return (typeof v==='number'?v.toFixed(2)+'%':v); }} }},
    legend: {{ data: DATA.runs.map(function(r){{ return r.name; }}), bottom: 36 }},
    grid: {{ left:60, right:20, top:40, bottom:80 }},
    xAxis: {{ type:'category', data: allDates, boundaryGap:false }},
    yAxis: {{ type:'value', name:'%', axisLabel:{{ formatter:function(v){{return v+'%';}} }} }},
    dataZoom: commonDataZoom,
    series: ddSeries
  }});

  window.addEventListener('resize', function() {{ c1.resize(); c2.resize(); }});
}})();
</script>
</body>
</html>
"#,
        start = meta.start,
        end = meta.end,
        n = n,
        table_rows = table_rows,
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

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    fn make_summary() -> Summary {
        Summary {
            total_contributed: 2000.0,
            final_equity: 4000.0,
            total_return: 1.0,
            annualized: 0.5,
            max_drawdown: 0.1,
            sharpe: 1.5,
            trade_count: 2,
        }
    }

    fn make_daily() -> Vec<DailyRecord> {
        vec![
            DailyRecord { date: d(2024, 1, 1), nav: 1.0, adj_nav: 1.0, equity: 1000.0, contribution: 1000.0, shares: 1000.0, cash: 0.0 },
            DailyRecord { date: d(2024, 2, 1), nav: 1.0, adj_nav: 1.0, equity: 2000.0, contribution: 1000.0, shares: 2000.0, cash: 0.0 },
            DailyRecord { date: d(2024, 2, 15), nav: 2.0, adj_nav: 2.0, equity: 4000.0, contribution: 0.0, shares: 2000.0, cash: 0.0 },
        ]
    }

    #[test]
    fn render_compare_html_returns_markup() {
        let runs = vec![RunOutcome {
            name: "甲".to_string(), fund_code: "161725".to_string(),
            summary: make_summary(), daily: make_daily(),
        }];
        let meta = CompareMeta { start: d(2024,1,1), end: d(2024,2,15) };
        let html = render_compare_html(&meta, &runs);
        assert!(html.contains("const DATA"));
        assert!(html.contains("甲"));
        assert!(html.contains("echarts"));
    }

    #[test]
    fn renders_compare_with_two_runs() {
        let runs = vec![
            RunOutcome {
                name: "普通定投".to_string(),
                fund_code: "161725".to_string(),
                summary: make_summary(),
                daily: make_daily(),
            },
            RunOutcome {
                name: "均线择时".to_string(),
                fund_code: "161725".to_string(),
                summary: Summary {
                    total_contributed: 2000.0,
                    final_equity: 3000.0,
                    total_return: 0.5,
                    annualized: 0.25,
                    max_drawdown: 0.2,
                    sharpe: 0.8,
                    trade_count: 5,
                },
                daily: make_daily(),
            },
        ];
        let meta = CompareMeta { start: d(2024, 1, 1), end: d(2024, 2, 15) };
        let tmp = std::env::temp_dir().join("xlh_compare_test");
        let path = render_compare(&meta, &runs, &tmp).unwrap();

        assert!(path.exists(), "compare.html should exist");
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("普通定投"), "should contain run name 普通定投");
        assert!(content.contains("均线择时"), "should contain run name 均线择时");
        assert!(content.contains("const DATA"), "should embed const DATA");
        assert!(content.contains("总收益"), "should contain 总收益");
        assert!(content.contains("最大回撤"), "should contain 最大回撤");
        assert!(content.contains("echarts"), "should reference echarts");
        assert!(content.contains("<table"), "should contain table");

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
