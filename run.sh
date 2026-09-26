for layer in 15_20 15_300 300_700 700_1000 700_1850 1800_1850
do
  # config ----
  meandir=/scratch/alpine/wimi7695/ohc_prod/potential_temperature/OP20260507/${layer}/OP20260507_potentialTemperature_-89.5S_89.5N_20.5W_379.5E_2004_2025_${layer}/Results/FullField
  ensembledir=/scratch/alpine/wimi7695/ohc_prod/potential_temperature/OP20260507/${layer}/OP20260507_potentialTemperature_-89.5S_89.5N_20.5W_379.5E_2004_2025_${layer}/Results/FullFieldLocalCondSim
  outputdir=/scratch/alpine/wimi7695/ohc_prod/results_260908
  tag=260908-OP20260507
  config=config.toml
  preset=wmo_wet        # publish mask policy (see mask_spec.md)
  experiment=C          # ME4OH experiment letter; leave empty for a product that isn't an ME4OH submission
  provenance=https://github.com/ocean-grid-processing/provenance/blob/main/README.md  # record of interesting facts about $tag
  codeversion=https://github.com/ocean-grid-processing/localgp_ohc_ingest/releases/tag/1.0.0
  # end config ----
  name=$(sed -n 's/^name *= *"\([^"]*\)".*/\1/p' $config); name=${name:-ohc}
  declare ingest=$(sbatch --parsable ohc_ingest.slurm $meandir $ensembledir $outputdir $layer $tag $provenance $codeversion $config)
  sbatch --dependency afterok:$ingest verify_store.slurm $outputdir $meandir $ensembledir $tag $layer
  declare publish=$(sbatch --parsable --dependency afterok:$ingest publish.slurm $outputdir $tag $layer $provenance $codeversion $preset "$experiment")
  sbatch --dependency afterok:$publish verify_publish.slurm "$outputdir/${name^^}_${tag}_*_lev${layer}[._]*" $meandir $ensembledir
done






