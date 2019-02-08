import os

from mail2jekyll import configuration, post, web


def start_server():
    # TODO: this sucks
    config_file = os.environ.get(
        'MAIL2JEKYLL_CONFIG',
        './mail2jekyll.toml'
    )

    config = configuration.load(config_file)
    queue = post.ContentQueue(config)

    web.create_app(config, queue)\
       .run(**config.get('http', {}))


if __name__ == '__main__':
    start_server()