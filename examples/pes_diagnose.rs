use zigzag::{
    formats::read::read_stitch_file,
    reconstruct::{SatinRun, repeated_run::RepeatedRun},
};
fn main() -> anyhow::Result<()> {
    for arg in std::env::args().skip(1) {
        let file = read_stitch_file(&std::fs::read(&arg)?, "pes")?;
        let runs:Vec<_>=file.blocks.iter().flat_map(|b| &b.runs).map(|p| {
 let spans=SatinRun::recover(p).map(|m|m.uncertain_spans()).unwrap_or(vec![(0,p.len()-1)]);
 let repeated=RepeatedRun::recover(p);
 let model=zigzag::reconstruct::StitchRun::recover(p);
 serde_json::json!({"model_counts":model.as_ref().map(|m|(m.fill_count(),m.satin_count(),m.repeated_count())), "model_uncertain":model.as_ref().map(|m|m.uncertain_spans()),"resized":model.as_ref().map(|m|[0.5,2.0].map(|scale|m.generate(zigzag::geom::scale_about(zigzag::geom::P::default(),scale,scale)).iter().map(|p|[p.x,p.y]).collect::<Vec<_>>())), "repeated":repeated.as_ref().map(|m|serde_json::json!({"coverage":m.covered_fraction(),"uncertain":m.uncertain_spans(),"blocks":m.blocks.iter().map(|b|(b.start,b.end,b.passes)).collect::<Vec<_>>()})),"points":p.iter().map(|p|[p.x,p.y]).collect::<Vec<_>>(),"uncertain":spans})
 }).collect();
        println!("{}", serde_json::json!({"file":arg,"runs":runs}));
    }
    Ok(())
}
