import logging
import os
from flask import Flask, render_template

from config import Config
from controllers.item_controller import item_bp
from controllers.cluster_controller import cluster_bp
from models.database import init_db

logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s [%(levelname)s] %(name)s: %(message)s",
)
logger = logging.getLogger("pgvisor_demo")


def create_app() -> Flask:
    """Create and configure the Flask application."""
    base_dir = os.path.abspath(os.path.dirname(__file__))
    views_dir = os.path.join(base_dir, "views")
    static_dir = os.path.join(base_dir, "static")

    app = Flask(
        __name__,
        template_folder=views_dir,
        static_folder=static_dir,
    )
    app.config.from_object(Config)

    # Register Controller Blueprints
    app.register_blueprint(item_bp)
    app.register_blueprint(cluster_bp)

    # Error handlers
    @app.errorhandler(404)
    def not_found(e):
        return render_template("base.html", error_title="404 - Not Found", error_message="Page not found"), 404

    @app.errorhandler(500)
    def server_error(e):
        return render_template("base.html", error_title="500 - Internal Error", error_message=str(e)), 500

    # Ensure demo database schema is ready
    try:
        init_db()
    except Exception as e:
        logger.warning("Initial database schema creation deferred: %s", e)

    return app


app = create_app()

if __name__ == "__main__":
    port = Config.PORT
    debug = Config.DEBUG
    logger.info("Starting PgVisor Demo App on 0.0.0.0:%d (debug=%s)", port, debug)
    app.run(host="0.0.0.0", port=port, debug=debug)
