export function outputName(input: string): string {
  return input.replace(/\.[^.]+$/, "") + ".png";
}
