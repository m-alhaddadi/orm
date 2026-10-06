const supported = ["darwin-arm64", "linux-x64", "win32-x64"];
const target = `${process.platform}-${process.arch}`;
if (!supported.includes(target)) throw new Error(`Unsupported ORM native platform ${target}; build from source`);
module.exports = require(`@orm/native-combined-${target}`);
