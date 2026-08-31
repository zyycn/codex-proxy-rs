import type { LineSeriesOption } from 'echarts'

type TemporalValue = number | null | undefined

interface GapRun {
  startIndex: number
  endIndex: number
}

interface GapBridgeOptions {
  name: string
  color: string
  xAxisIndex?: number
  yAxisIndex?: number
  width?: number
  z?: number
  maxGapBuckets?: number
}

const defaultShortGapBuckets = 2

export function sampleGapBridgeSeries(
  values: TemporalValue[],
  options: GapBridgeOptions,
): LineSeriesOption[] {
  const data = values.map(normalizeValue)
  const maxGapBuckets = options.maxGapBuckets ?? defaultShortGapBuckets
  const series: LineSeriesOption[] = []

  for (const [runIndex, run] of boundedGapRuns(data)
    .filter(run => gapLength(run) <= maxGapBuckets)
    .entries()) {
    const beforeIndex = run.startIndex - 1
    const afterIndex = run.endIndex + 1
    const beforeValue = data[beforeIndex]
    const afterValue = data[afterIndex]
    const bridge = Array.from<number | null>({ length: data.length }).fill(null)

    if (beforeValue == null || afterValue == null)
      continue

    const span = afterIndex - beforeIndex
    for (let index = beforeIndex; index <= afterIndex; index += 1) {
      const progress = (index - beforeIndex) / span
      bridge[index] = beforeValue + (afterValue - beforeValue) * progress
    }

    series.push({
      name: `${options.name}-gap-${runIndex}`,
      type: 'line',
      data: bridge,
      connectNulls: false,
      smooth: false,
      showSymbol: false,
      silent: true,
      xAxisIndex: options.xAxisIndex ?? 0,
      yAxisIndex: options.yAxisIndex ?? 0,
      z: Math.max(0, (options.z ?? 2) - 1),
      lineStyle: {
        color: options.color,
        type: 'dashed',
        width: Math.max(1, (options.width ?? 2.2) - 0.7),
        opacity: 0.32,
      },
      itemStyle: { color: options.color, opacity: 0 },
      emphasis: { disabled: true },
      tooltip: { show: false },
    })
  }

  return series
}

export function requestActivityByBucket(
  buckets: string[],
  activity: Array<{ bucket: string, totalRequests: number }>,
) {
  const requestsByBucket = new Map(
    activity.map(point => [point.bucket, Math.max(0, point.totalRequests)]),
  )
  return buckets.map(bucket => (requestsByBucket.get(bucket) ?? 0) > 0)
}

export function zeroInactiveValues(
  values: TemporalValue[],
  active: boolean[],
) {
  return values.map((value, index) => normalizeValue(value) ?? (active[index] === false ? 0 : null))
}

function normalizeValue(value: TemporalValue) {
  return typeof value === 'number' && Number.isFinite(value) ? value : null
}

function boundedGapRuns(values: Array<number | null>) {
  return gapRuns(values.map(value => value == null)).filter(
    run => run.startIndex > 0 && run.endIndex < values.length - 1,
  )
}

function gapRuns(isGap: boolean[]) {
  const runs: GapRun[] = []
  let startIndex = -1

  for (let index = 0; index <= isGap.length; index += 1) {
    if (isGap[index] && startIndex < 0) {
      startIndex = index
      continue
    }
    if (isGap[index] || startIndex < 0)
      continue
    runs.push({ startIndex, endIndex: index - 1 })
    startIndex = -1
  }

  return runs
}

function gapLength(run: GapRun) {
  return run.endIndex - run.startIndex + 1
}
