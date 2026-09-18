from flask import Blueprint, flash, redirect, render_template, request, url_for

from models.cluster import ClusterModel
from models.item import DemoItem

item_bp = Blueprint("items", __name__)


@item_bp.route("/")
def index():
    """Display all items with filter, search, summary cards, and cluster status banner."""
    status_filter = request.args.get("status", "all")
    search = request.args.get("q", "").strip()

    try:
        items = DemoItem.all(status_filter=status_filter, search=search)
        counts = DemoItem.count_by_status()
        cluster_info = ClusterModel.get_cluster_info()
    except Exception as e:
        flash(f"Error connecting to database: {e}", "danger")
        items = []
        counts = {"Active": 0, "Pending": 0, "Archived": 0, "Total": 0}
        cluster_info = {"connected": False, "error": str(e)}

    return render_template(
        "items/index.html",
        items=items,
        counts=counts,
        cluster_info=cluster_info,
        status_filter=status_filter,
        search=search,
    )


@item_bp.route("/items/create", methods=["POST"])
def create():
    """Create a new demo item."""
    title = request.form.get("title", "").strip()
    category = request.form.get("category", "General").strip()
    description = request.form.get("description", "").strip()
    status = request.form.get("status", "Active").strip()

    if not title:
        flash("Title is required.", "warning")
        return redirect(url_for("items.index"))

    try:
        created = DemoItem.create(title=title, category=category, description=description, status=status)
        flash(f"Item #{created.id} '{created.title}' created successfully!", "success")
    except Exception as e:
        flash(f"Failed to create item: {e}", "danger")

    return redirect(url_for("items.index"))


@item_bp.route("/items/<int:item_id>/edit", methods=["GET"])
def edit(item_id: int):
    """Render the edit form for a single item."""
    try:
        item = DemoItem.find(item_id)
        if not item:
            flash(f"Item #{item_id} not found.", "warning")
            return redirect(url_for("items.index"))
        cluster_info = ClusterModel.get_cluster_info()
        return render_template("items/edit.html", item=item, cluster_info=cluster_info)
    except Exception as e:
        flash(f"Error retrieving item: {e}", "danger")
        return redirect(url_for("items.index"))


@item_bp.route("/items/<int:item_id>/edit", methods=["POST"])
def update(item_id: int):
    """Update an existing item."""
    title = request.form.get("title", "").strip()
    category = request.form.get("category", "General").strip()
    description = request.form.get("description", "").strip()
    status = request.form.get("status", "Active").strip()

    if not title:
        flash("Title is required.", "warning")
        return redirect(url_for("items.edit", item_id=item_id))

    try:
        success = DemoItem.update(item_id=item_id, title=title, category=category, description=description, status=status)
        if success:
            flash(f"Item #{item_id} updated successfully.", "success")
        else:
            flash(f"Item #{item_id} was not found or updated.", "warning")
    except Exception as e:
        flash(f"Failed to update item: {e}", "danger")

    return redirect(url_for("items.index"))


@item_bp.route("/items/<int:item_id>/delete", methods=["POST"])
def delete(item_id: int):
    """Delete an item."""
    try:
        success = DemoItem.delete(item_id)
        if success:
            flash(f"Item #{item_id} deleted.", "info")
        else:
            flash(f"Item #{item_id} not found.", "warning")
    except Exception as e:
        flash(f"Failed to delete item: {e}", "danger")

    return redirect(url_for("items.index"))


@item_bp.route("/items/seed", methods=["POST"])
def seed():
    """Seed sample items."""
    try:
        count = DemoItem.seed_defaults()
        flash(f"Successfully seeded {count} sample items.", "success")
    except Exception as e:
        flash(f"Failed to seed items: {e}", "danger")

    return redirect(url_for("items.index"))
