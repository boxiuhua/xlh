use std::path::Path;
use anyhow::{anyhow, Result};
use plotters::prelude::*;
use crate::portfolio::Portfolio;

/// 把权益曲线画成 PNG，保存到 out_dir/equity.png。
pub fn render_equity(pf: &Portfolio, out_dir: &Path) -> Result<()> {
    if pf.curve.is_empty() { return Err(anyhow!("无数据可画图")); }
    std::fs::create_dir_all(out_dir).ok();
    let path = out_dir.join("equity.png");

    let path_str = path.to_str().ok_or_else(|| anyhow!("输出路径含非法字符: {}", path.display()))?;
    let root = BitMapBackend::new(path_str, (1000, 600)).into_drawing_area();
    root.fill(&WHITE)?;

    let n = pf.curve.len();
    let y_min = pf.curve.iter().map(|p| p.equity).fold(f64::MAX, f64::min);
    let y_max = pf.curve.iter().map(|p| p.equity).fold(f64::MIN, f64::max);
    let pad = (y_max - y_min).max(1.0) * 0.05;

    let mut chart = ChartBuilder::on(&root)
        .caption("Equity Curve", ("sans-serif", 28))
        .margin(20)
        .x_label_area_size(40)
        .y_label_area_size(60)
        .build_cartesian_2d(0..n, (y_min - pad)..(y_max + pad))?;

    chart.configure_mesh().draw()?;
    chart.draw_series(LineSeries::new(
        pf.curve.iter().enumerate().map(|(i, p)| (i, p.equity)),
        &BLUE,
    ))?;

    root.present()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    fn d(y:i32,m:u32,day:u32)->NaiveDate{NaiveDate::from_ymd_opt(y,m,day).unwrap()}

    #[test]
    fn renders_png_file() {
        let mut pf = Portfolio::new(0.0);
        pf.curve.push(crate::portfolio::EquityPoint{date:d(2024,1,1),equity:100.0,contribution:100.0});
        pf.curve.push(crate::portfolio::EquityPoint{date:d(2024,1,2),equity:110.0,contribution:0.0});
        let dir = std::env::temp_dir().join("xlh_chart_test");
        render_equity(&pf, &dir).unwrap();
        assert!(dir.join("equity.png").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
