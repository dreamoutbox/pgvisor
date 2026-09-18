from flask import Blueprint, jsonify, render_template, request

from models.cluster import ClusterModel

cluster_bp = Blueprint("cluster", __name__)


@cluster_bp.route("/cluster")
def index():
    """Display cluster inspection and read/write splitting diagnostics."""
    cluster_info = ClusterModel.get_cluster_info()
    routing_results = None
    if request.args.get("run_test") == "1":
        routing_results = ClusterModel.test_routing()

    return render_template(
        "cluster/index.html",
        cluster_info=cluster_info,
        routing_results=routing_results,
    )


@cluster_bp.route("/api/health")
def health():
    """Healthcheck endpoint for container orchestration and monitoring."""
    info = ClusterModel.get_cluster_info()
    status_code = 200 if info.get("connected") else 503
    return jsonify({
        "status": "healthy" if info.get("connected") else "unhealthy",
        "database": info,
    }), status_code
