//! Read-only interactive audio diagnostic. All guest writes stay in the given overlay.
//! audio_transition_probe <EXE> <jit|interpreter|unicorn> <SECONDS> <OVERLAY>
//! stdin: click X Y, down VK, up VK, capture PATH, report PATH, close.
#[path="../tests/support/cpu.rs"] mod backend;
use adv_runtime::boot::Runtime;
use pal_machine::{RunResult,SliceResult};
use std::{io::BufRead,time::{Duration,Instant},collections::BTreeMap};

fn run()->Result<(),String> {
    let args:Vec<_>=std::env::args().skip(1).collect();
    if args.len()!=4 {return Err("audio_transition_probe <EXE> <jit|interpreter|unicorn> <SECONDS> <OVERLAY>".into());}
    let mut r=Runtime::with_cpu(&args[0],&args[3],backend::Cpu::new(&args[1])?,800,600)?;
    r.set_codepage(936);r.set_audio_enabled(true);r.set_auto_dialog_ms(Some(50));r.enable_audio_pcm_digests();
    let seconds=args[2].parse::<u64>().map_err(|_|"invalid seconds")?;
    let started=Instant::now();let (tx,rx)=std::sync::mpsc::channel();
    let mut previous_files=BTreeMap::new();
    std::thread::spawn(move || for line in std::io::stdin().lock().lines() {let Ok(line)=line else {break};if tx.send(line).is_err(){break;}});
    while started.elapsed()<Duration::from_secs(seconds) {
        let files:BTreeMap<_,_>=r.machine.ext.get::<pal_win32::Win32>().files.iter()
            .map(|(&id,file)|(id,file.guest_path.clone())).collect();
        if files!=previous_files {println!("files us={} {files:?}",r.machine.sys.clock.now_us());previous_files=files;}
        for line in rx.try_iter() {
            let (cmd,rest)=line.split_once(' ').unwrap_or((&line,""));
            match cmd {
                "click"=>{
                    let mut words=rest.split_whitespace();let x=words.next().ok_or("missing x")?.parse::<u16>().map_err(|_|"bad x")?;
                    let y=words.next().ok_or("missing y")?.parse::<u16>().map_err(|_|"bad y")?;
                    let lp=u32::from(x)|(u32::from(y)<<16);r.post_input(0x201,1,lp);r.post_input(0x202,0,lp);
                }
                "down"|"up"=>{let vk=rest.parse::<u32>().map_err(|_|"bad vk")?;
                    if vk>255 {return Err("vk exceeds255".into());}
                    r.post_input(if cmd=="down"{0x100}else{0x101},vk,if cmd=="down"{1}else{0xc0000001});
                }
                "report"=>std::fs::write(rest,r.report()).map_err(|e|e.to_string())?,
                "capture"=>{
                    let frame=r.frame();let mut rgba=frame.bgra.clone();
                    for pixel in rgba.as_chunks_mut::<4>().0 {pixel.swap(0,2);pixel[3]=255;}
                    let file=std::fs::File::create(rest).map_err(|e|e.to_string())?;
                    let mut enc=png::Encoder::new(file,frame.width,frame.height);enc.set_color(png::ColorType::Rgba);
                    enc.write_header().map_err(|e|e.to_string())?.write_image_data(&rgba).map_err(|e|e.to_string())?;
                }
                "close"=>return Ok(()),_=>return Err(format!("unknown command {cmd}")),
            }
            println!("ok {line}");
        }
        match r.step_for(Duration::from_millis(8)) {
            SliceResult::Complete(RunResult::Failed(e))=>return Err(format!("{e}\n{}",r.report())),
            SliceResult::Complete(RunResult::Exited(code))=>{println!("exit {code}");return Ok(());}
            SliceResult::Complete(RunResult::Idle{next_deadline})=>{
                let now=r.machine.sys.clock.now_us();let delay=next_deadline.map_or(10_000,|at|at.saturating_sub(now).min(10_000));
                std::thread::sleep(Duration::from_micros(delay));
            }
            SliceResult::Yielded=>{},
        }
    }
    println!("{}",r.report());Ok(())
}
fn main(){if let Err(e)=run(){eprintln!("{e}");std::process::exit(1);}}
