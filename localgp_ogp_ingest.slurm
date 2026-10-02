#!/bin/bash
#SBATCH --qos=preemptable
#SBATCH --export=NONE
#SBATCH --nodes=1
#SBATCH --ntasks-per-node=1
#SBATCH --time=12:00:00
#SBATCH --job-name=localgp_ogp_ingest
#SBATCH --output=%x.%j.out
#SBATCH --mail-type=FAIL
#SBATCH --mail-user=katie.mills@colorado.edu
#SBATCH --mem=32GB
#SBATCH --exclude=blanca-g6-u21

module purge

cd $SLURM_SUBMIT_DIR
./dist/localgp_ogp_ingest $8 \
  --dir_mean $1 --dir_ensemble $2 --dir_out $3 --layer $4 --tag $5 --provenance-link $6 --code-version $7


