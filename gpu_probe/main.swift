// lmforge-gpu-probe — Apple Silicon GPU statistics reader
//
// Reads GPU utilisation and Metal memory via IOAccelerator PerformanceStatistics.
// On AGX (Apple Silicon), the key is "Device Utilization %" (not "GPU Activity(%)").
// Reference: https://github.com/exelban/stats Modules/GPU/reader.swift:120
//
// Compile:
//   swiftc -O -o lmforge-gpu-probe main.swift -framework IOKit -framework Foundation

import Foundation
import IOKit

// ── Output ────────────────────────────────────────────────────────────────────

struct GPUProbeResult: Codable {
    var gpu_util_pct:     Double?
    var gpu_mem_used_mb:  Double?
    var gpu_mem_total_mb: Double?
    var source:           String
}

func emit(_ r: GPUProbeResult) -> Never {
    if let data = try? JSONEncoder().encode(r),
       let str  = String(data: data, encoding: .utf8) { print(str) }
    exit(0)
}

// ── IOAccelerator reader ──────────────────────────────────────────────────────

func readAccelerator() -> GPUProbeResult? {
    guard let match = IOServiceMatching("IOAccelerator") else { return nil }
    var iter: io_iterator_t = 0
    guard IOServiceGetMatchingServices(kIOMainPortDefault, match, &iter) == kIOReturnSuccess else { return nil }
    defer { IOObjectRelease(iter) }

    let toInt: (Any) -> Int? = {
        ($0 as? Int) ?? ($0 as? NSNumber)?.intValue
    }
    let toMB: (Any) -> Double? = {
        if let d = $0 as? Double   { return d / 1_048_576.0 }
        if let i = $0 as? Int      { return Double(i) / 1_048_576.0 }
        if let n = $0 as? NSNumber { return n.doubleValue / 1_048_576.0 }
        return nil
    }

    var svc = IOIteratorNext(iter)
    while svc != IO_OBJECT_NULL {
        defer { IOObjectRelease(svc); svc = IOIteratorNext(iter) }

        var pr: Unmanaged<CFMutableDictionary>? = nil
        guard IORegistryEntryCreateCFProperties(svc, &pr, kCFAllocatorDefault, 0) == kIOReturnSuccess,
              let props = pr?.takeRetainedValue() as? [String: Any],
              let perf  = props["PerformanceStatistics"] as? [String: Any]
        else { continue }

        // Exactly what exelban/stats does on line 120:
        // AGX (Apple Silicon) → "Device Utilization %"
        // Intel/AMD           → "GPU Activity(%)"
        // Renderer fallback   → "Renderer Utilization %"
        var utilPct: Double? = nil
        if let v = perf["Device Utilization %"] ?? perf["GPU Activity(%)"] ?? perf["Renderer Utilization %"], let i = toInt(v) {
            utilPct = Double(min(i, 100))
        }

        let usedMB  = perf["In use system memory"].flatMap(toMB)
        let totalMB = perf["Allocated system memory"].flatMap(toMB)

        if utilPct != nil || usedMB != nil {
            return GPUProbeResult(
                gpu_util_pct:     utilPct,
                gpu_mem_used_mb:  usedMB,
                gpu_mem_total_mb: totalMB,
                source: "IOAccelerator"
            )
        }
    }
    return nil
}

// ── Main ──────────────────────────────────────────────────────────────────────

if let result = readAccelerator() {
    emit(result)
}
emit(GPUProbeResult(gpu_util_pct: nil, gpu_mem_used_mb: nil, gpu_mem_total_mb: nil, source: "unavailable"))
