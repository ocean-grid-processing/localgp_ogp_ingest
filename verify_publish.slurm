#!/bin/bash
#SBATCH --qos=preemptable
#SBATCH --export=NONE
#SBATCH --nodes=1
#SBATCH --ntasks-per-node=1
#SBATCH --time=12:00:00
#SBATCH --job-name=verify_publish
#SBATCH --output=%x.%j.out
#SBATCH --mail-type=FAIL
#SBATCH --mail-user=katie.mills@colorado.edu
#SBATCH --mem=32GB
#SBATCH --exclude=blanca-g6-u21

module purge
module load anaconda/2023.09
conda activate ohc_crosscheck

cd $SLURM_SUBMIT_DIR
# $1 is a filename pattern for the submission (run.sh builds it from the quantity name, tag and layer);
# it must match exactly one file — results dirs hold many runs, so anything else is an error, not a guess.
shopt -s nullglob
subs=( $1 )
if [ ${#subs[@]} -ne 1 ]; then
  echo "expected exactly one submission matching $1; found ${#subs[@]}: ${subs[*]}" >&2
  exit 1
fi
python scripts/verify_publish.py "${subs[0]}" $2 $3
