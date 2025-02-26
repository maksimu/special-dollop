# Build

## Wheel Files

```shell
maturin build --release
pip install pam_rustwebrtc*                                                                          
```
If using a different chip set you can use docker to make it

```shell                                                                                 
docker run --rm --platform linux/amd64 -v $(pwd):/io ghcr.io/pyo3/maturin build --release --manylinux 2014
pip install pam_rustwebrtc*   
```
