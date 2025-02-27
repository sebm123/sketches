const path = require('path');
const CopyWebpackPlugin = require('copy-webpack-plugin');

module.exports = {
    entry: {
        background: './src/js/background.js',
        content: './src/js/content.js',
        view_recipe: './src/js/view_recipe.js',
        welcome: './src/js/welcome.js'
    },

    output: {
        path: path.resolve(__dirname, 'addon'),
        filename: '[name].js'
    },

    module: {
        rules: [
            {
                test: /\.js$/,
                exclude: /node_modules/,
                use: {loader: 'babel-loader'}
            }
        ]
    },

    plugins: [
        new CopyWebpackPlugin([
            {from: './src/icons/', to: path.resolve(__dirname, 'addon', 'icons')},
            {from: './src/images/', to: path.resolve(__dirname, 'addon', 'images')},
            {from: './src/manifest.json'},
            {from: './src/html/'}
        ])
    ]
};
