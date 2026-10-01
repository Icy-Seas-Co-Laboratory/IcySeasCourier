const buildChip = document.querySelector("#registry-build");

fetch("/api/v1/version", { cache: "no-store" })
  .then((response) => {
    if (!response.ok) throw new Error("Registry version request failed");
    return response.json();
  })
  .then(({ version, build }) => {
    buildChip.textContent = `Registry v${version} · build ${build}`;
  })
  .catch(() => {
    buildChip.textContent = "Registry build info unavailable";
  });
