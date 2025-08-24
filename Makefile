.PHONY: docker-image-askld
docker-image-askld:
	nix build .#askld-image
	docker load < ./result

docker-image: docker-image-askld
.PHONY: docker-image

.PHONY: docker-run
docker-run:
	@docker run -d --rm --name askld -e RUST_LOG=info -p 8080:80 -v "$$(pwd)/../index:/data/:rw" askld:latest

docker-stop:
	docker stop askld
.PHONY: docker-stop

