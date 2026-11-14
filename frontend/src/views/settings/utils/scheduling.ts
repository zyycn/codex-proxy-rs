export function validSchedulingWeight(input: string) {
  const value = Number(input)
  return input.trim() !== '' && Number.isFinite(value) && value >= 0 && value <= 10 && Math.round(value * 10) / 10 === value
}
