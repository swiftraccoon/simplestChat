# Reviewed subset of ansible-core 2.21.4 templating; results remain untrusted.

from collections.abc import Mapping

from ansible.parsing.dataloader import DataLoader

class Templar:
    # Real Ansible template evaluation with caller-validated output.

    def __init__(
        self, loader: DataLoader | None = None, variables: Mapping[str, object] | None = None
    ) -> None: ...
    # Results may be arbitrary rendered data, not an assumed string or JSON shape.
    def template(self, variable: object) -> object: ...

# Callers must restrict trusted templates to reviewed, checked-in source text.
def trust_as_template(value: str) -> str: ...
