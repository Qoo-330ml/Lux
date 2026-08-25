target "runtime" {
  context    = "."
  dockerfile = "runtime/Dockerfile"
}

target "app" {
  context    = "."
  dockerfile = "Dockerfile"

  contexts = {
    "lux-runtime" = "target:runtime"
  }

  args = {
    LUX_RUNTIME_IMAGE = "lux-runtime"
  }
}
