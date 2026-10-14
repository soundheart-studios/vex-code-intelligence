if (process.env.X) {
  throw new Error("x");
}
switch (process.argv.length) {
  case 1:
    console.log("one");
}
