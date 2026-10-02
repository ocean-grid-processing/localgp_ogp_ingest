#!/bin/bash
#SBATCH --qos=preemptable
#SBATCH --export=NONE
#SBATCH --nodes=1
#SBATCH --ntasks-per-node=1
#SBATCH --time=12:00:00
#SBATCH --job-name=publish_for_ohc
#SBATCH --output=%x.%j.out
#SBATCH --mail-type=FAIL
#SBATCH --mail-user=katie.mills@colorado.edu
#SBATCH --mem=64GB
#SBATCH --exclude=blanca-g6-u21

module purge
module load anaconda/2023.09
conda activate ohc_crosscheck

cd $SLURM_SUBMIT_DIR
# args: OUTPUT_DIR TAG LAYER PROVENANCE_LINK CODE_VERSION PRESET [EXPERIMENT]
# the store name is <quantity>_<tag>_<Y0>_<Y1>_plev<layer>.zarr (the data span is discovered); glob it by tag and layer.
# An empty EXPERIMENT means no --experiment (the product isn't an ME4OH submission).
python scripts/publish.py ${1}/*_${2}_*_plev${3}.zarr --preset $6 ${7:+--experiment $7} --out $1 --ensemble --provenance-link $4 --code-version $5
