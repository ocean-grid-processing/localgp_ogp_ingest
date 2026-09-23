#!/bin/bash
#SBATCH --qos=preemptable
#SBATCH --export=NONE
#SBATCH --nodes=1
#SBATCH --ntasks-per-node=1
#SBATCH --time=12:00:00
#SBATCH --job-name=verify_store
#SBATCH --output=%x.%j.out
#SBATCH --mail-type=FAIL
#SBATCH --mail-user=katie.mills@colorado.edu
#SBATCH --mem=32GB
#SBATCH --exclude=blanca-g6-u21

module purge
module load anaconda/2023.09
conda activate ohc_crosscheck

cd $SLURM_SUBMIT_DIR
# the store name is <quantity>_<tag>_<Y0>_<Y1>_plev<layer>.zarr (the data span is discovered); glob it by tag and layer
python scripts/verify_store.py ${1}/*_${4}_*_plev${5}.zarr $2 $3


