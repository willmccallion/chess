//! Trains the engine's own network with bullet. The architecture and quantisation come from
//! `chess::eval::own`, so a checkpoint's `quantised.bin` loads with `--eval own --own-net`.

use bullet::game::inputs::Chess768;
use bullet::nn::optimiser::AdamW;
use bullet::trainer::save::SavedFormat;
use bullet::trainer::schedule::{TrainingSchedule, TrainingSteps, lr, wdl};
use bullet::trainer::settings::LocalSettings;
use bullet::value::ValueTrainerBuilder;
use bullet::value::loader::DirectSequentialDataLoader;
use chess::eval::own::{EVAL_SCALE, HIDDEN, INPUTS, QA, QB};
use clap::Parser;

const BATCH_SIZE: usize = 16_384;
/// About 100M positions per superbatch.
const BATCHES_PER_SUPERBATCH: usize = 6104;
/// The cosine schedule ends at this fraction of the starting learning rate.
const FINAL_LR_FRACTION: f32 = 0.3 * 0.3 * 0.3 * 0.3 * 0.3;

#[derive(Parser)]
#[command(about = "Train the engine's own network on shuffled bulletformat data")]
struct Args {
    /// Shuffled bulletformat files, made from `chess datagen` output by `bullet-utils`
    #[arg(required = true)]
    data: Vec<String>,
    /// Name of this network; checkpoints are written as <net-id>-<superbatch>
    #[arg(long)]
    net_id: String,
    #[arg(long, default_value_t = 40)]
    superbatches: usize,
    /// Share of each target taken from the game result rather than the search score
    #[arg(long, default_value_t = 0.75)]
    wdl: f32,
    /// Starting learning rate, decayed along a cosine to a small fraction of it
    #[arg(long, default_value_t = 0.001)]
    lr: f32,
    /// Save a checkpoint every this many superbatches (and always after the last)
    #[arg(long, default_value_t = 10)]
    save_rate: usize,
    #[arg(long, default_value = "checkpoints")]
    out: String,
    /// Threads that load data while the GPU trains
    #[arg(long, default_value_t = 4)]
    threads: usize,
}

fn main() {
    let args = Args::parse();

    let mut trainer = ValueTrainerBuilder::default()
        .dual_perspective()
        .optimiser(AdamW)
        .inputs(Chess768)
        .save_format(&[
            SavedFormat::id("l0w").round().quantise::<i16>(QA),
            SavedFormat::id("l0b").round().quantise::<i16>(QA),
            SavedFormat::id("l1w").round().quantise::<i16>(QB),
            SavedFormat::id("l1b").round().quantise::<i16>(QA * QB),
        ])
        .loss_fn(|output, target| output.sigmoid().squared_error(target))
        .build(|builder, stm_inputs, ntm_inputs| {
            let l0 = builder.new_affine("l0", INPUTS, HIDDEN);
            let l1 = builder.new_affine("l1", 2 * HIDDEN, 1);
            let stm_hidden = l0.forward(stm_inputs).screlu();
            let ntm_hidden = l0.forward(ntm_inputs).screlu();
            l1.forward(stm_hidden.concat(ntm_hidden))
        });

    let schedule = TrainingSchedule {
        net_id: args.net_id,
        eval_scale: EVAL_SCALE as f32,
        steps: TrainingSteps {
            batch_size: BATCH_SIZE,
            batches_per_superbatch: BATCHES_PER_SUPERBATCH,
            start_superbatch: 1,
            end_superbatch: args.superbatches,
        },
        wdl_scheduler: wdl::ConstantWDL { value: args.wdl },
        lr_scheduler: lr::CosineDecayLR {
            initial_lr: args.lr,
            final_lr: args.lr * FINAL_LR_FRACTION,
            final_superbatch: args.superbatches,
        },
        save_rate: args.save_rate,
    };
    let settings = LocalSettings {
        threads: args.threads,
        test_set: None,
        output_directory: &args.out,
        batch_queue_size: 64,
    };
    let paths: Vec<&str> = args.data.iter().map(String::as_str).collect();
    let data = DirectSequentialDataLoader::new(&paths);

    trainer.run(&schedule, &settings, &data);
}
