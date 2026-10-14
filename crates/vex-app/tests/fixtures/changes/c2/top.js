if (process.env.X) {
  throw new Error("x");
}
if (process.env.Y) {
  return;
}
switch (process.argv.length) {
  case 2:
    console.log("two");
}
