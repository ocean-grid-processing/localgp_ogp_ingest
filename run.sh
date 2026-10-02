for layer in 15_20
do
  # config ----
  meandir=/scratch/alpine/wimi7695/mld_post/FullField
  ensembledir=/scratch/alpine/wimi7695/mld_post/FullFieldLocalCondSim
  outputdir=/scratch/alpine/wimi7695/mld_post/results-260921
  tag=260921-OP20260507
  config=config.toml
  preset=wmo_layerless        # publish mask policy (see mask_spec.md)
  experiment=          # ME4OH experiment letter; leave empty for a product that isn't an ME4OH submission
  provenance=https://github.com/ocean-grid-processing/provenance/blob/main/README.md  # record of interesting facts about $tag
  codeversion=https://github.com/ocean-grid-processing/localgp_ohc_ingest/releases/tag/1.1.0
  # end config ----
  name=$(sed -n 's/^name *= *"\([^"]*\)".*/\1/p' $config); name=${name:-ohc}
  declare ingest=$(sbatch --parsable localgp_ogp_ingest.slurm $meandir $ensembledir $outputdir $layer $tag $provenance $codeversion $config)
  sbatch --dependency afterok:$ingest verify_store.slurm $outputdir $meandir $ensembledir $tag $layer
  declare publish=$(sbatch --parsable --dependency afterok:$ingest publish.slurm $outputdir $tag $layer $provenance $codeversion $preset "$experiment")
  sbatch --dependency afterok:$publish verify_publish.slurm "$outputdir/${name^^}_${tag}_*_lev${layer}[._]*" $meandir $ensembledir
done






