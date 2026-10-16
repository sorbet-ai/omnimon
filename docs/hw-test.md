# Help verify NVLink, MIG and GPM

NVLink, MIG and GPM (tensor-core and memory-bandwidth activity) support is
written against NVIDIA's documented API, with constants and struct layouts
checked against NVIDIA's `nvidia-ml-py`, and exercised with `--demo`. It has
not been verified on real hardware yet. If you have access to such a machine,
the steps below take about 30 minutes and cover all three.

## What's needed

- 2 or more **SXM** A100 or H100 GPUs (PCIe cards usually have no NVLink).
  H100 or newer also exercises GPM.
- Root on the machine (enabling MIG needs it; most container hosts refuse).
- Driver r535 or newer, and `python3` with PyTorch for the load below.

## Steps

Install with `cargo install omnimon`, or download
`omnimon-x86_64-unknown-linux-gnu` from the latest
[release](https://github.com/sorbet-ai/omnimon/releases) and `chmod +x` it.
Then:

```sh
# 1. What was detected. Expect "GPM true, NVLink true" per GPU.
OMNIMON_LOG=debug omnimon --diagnose 2>&1 | grep -E "GPM|NVLink|MIG"
omnimon --json > idle.json

# 2. Reference values to compare against.
nvidia-smi nvlink -s          # link speeds / state
nvidia-smi nvlink -e          # error counters
nvidia-smi topo -m            # who is linked to whom

# 3. NVLink traffic: copy between GPU 0 and 1 in a loop, watch the
#    Details tab (NVLink block) and the NVL rates on the cards.
python3 - <<'EOF' &
import torch, time
a = torch.empty(1 << 28, device="cuda:0")
b = torch.empty(1 << 28, device="cuda:1")
t = time.time()
while time.time() - t < 120:
    b.copy_(a); a.copy_(b)
torch.cuda.synchronize()
EOF
omnimon          # Details tab on GPU 0; compare with: nvidia-smi nvlink -gt d

# 4. GPM / tensor cores (H100): a bf16 matmul loop should show high
#    "Tensor cores" (fp16/bf16) and memory bandwidth on the card.
python3 -c "
import torch, time
x = torch.randn(8192, 8192, device='cuda:0', dtype=torch.bfloat16)
t = time.time()
while time.time() - t < 60: x @ x
" &
omnimon

# 5. MIG on GPU 1 (needs root; may need a GPU reset or reboot).
sudo nvidia-smi -i 1 -mig 1
sudo nvidia-smi mig -i 1 -lgip            # list profiles and their ids
sudo nvidia-smi mig -i 1 -cgi <id>,<id> -C # e.g. a 3g and a 2g profile
nvidia-smi -L                              # MIG UUIDs
CUDA_VISIBLE_DEVICES=MIG-<uuid> python3 -c "
import torch, time
x = torch.randn(4096, 4096, device='cuda')
t = time.time()
while time.time() - t < 60: x @ x
" &
omnimon          # GPU 1 card shows MIG 2x; Details lists the partitions;
                   # the process shows as 1/<instance> in the table
omnimon --json > mig.json

# 6. Clean up.
sudo nvidia-smi mig -i 1 -dci && sudo nvidia-smi mig -i 1 -dgi
sudo nvidia-smi -i 1 -mig 0
```

Then [open an issue](https://github.com/sorbet-ai/omnimon/issues) with
`idle.json`, `mig.json`, the `--diagnose` output, and anything that looked
wrong next to the matching `nvidia-smi` numbers.
