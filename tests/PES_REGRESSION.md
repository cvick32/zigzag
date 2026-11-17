# PES reconstruction regression checks

The default test suite uses synthetic designs and the checked-in `examples/alower.pes` fixture. Downloaded PES collections are external test data and are not redistributed in this repository.

To run the ignored corpus tests, point at a collection containing the `DailyFilled`, `Handstitch`, and `Floss` directories:

```sh
ZIGZAG_PES_FIXTURES=/path/to/pes cargo test --lib reconstruct -- --ignored
```
